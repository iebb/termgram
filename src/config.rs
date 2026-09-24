use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;

const EMBEDDED_API_ID: Option<&str> = option_env!("TERMGRAM_EMBEDDED_API_ID");
const EMBEDDED_API_HASH: Option<&str> = option_env!("TERMGRAM_EMBEDDED_API_HASH");
const SETTINGS_FILE_NAME: &str = "settings.conf";
const SETTINGS_FORMAT_VERSION: &str = "1";
const MAX_SETTINGS_BYTES: u64 = 16 * 1024;
pub const MAX_ACCOUNTS: u8 = 8;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReleaseChannel {
    #[default]
    Stable,
    Prerelease,
}

impl ReleaseChannel {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stable => "Stable",
            Self::Prerelease => "Prerelease",
        }
    }

    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Stable => Self::Prerelease,
            Self::Prerelease => Self::Stable,
        }
    }

    const fn persisted(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Prerelease => "prerelease",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DownloadBehavior {
    /// Enter keeps the file in Termgram's managed cache. The separate
    /// explicit reveal action remains available.
    CacheOnly,
    /// A second explicit activation reveals the containing folder or selects
    /// the file; downloaded content is never executed directly.
    #[default]
    RevealOnActivation,
}

impl DownloadBehavior {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CacheOnly => "Keep in cache",
            Self::RevealOnActivation => "Reveal on activation",
        }
    }

    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::CacheOnly => Self::RevealOnActivation,
            Self::RevealOnActivation => Self::CacheOnly,
        }
    }

    const fn persisted(self) -> &'static str {
        match self {
            Self::CacheOnly => "cache_only",
            Self::RevealOnActivation => "reveal_on_activation",
        }
    }
}

/// Small, non-sensitive preferences stored in Termgram's platform config
/// directory. Telegram credentials and sessions are deliberately excluded.
///
/// The proxy URL may embed a username and password, so [`Debug`]
/// redacts the value instead of deriving it.
#[derive(Clone, Eq, PartialEq)]
pub struct Settings {
    pub automatic_update_checks: bool,
    pub release_channel: ReleaseChannel,
    pub download_behavior: DownloadBehavior,
    /// Show the inspected message identifier in the bottom message details.
    /// Reply details always show their target identifier
    /// regardless of this preference.
    pub show_message_ids: bool,
    /// One-based local session slot currently selected for Telegram.
    pub active_account: u8,
    /// Number of local session slots created by the user.
    pub account_count: u8,
    /// SOCKS5 or HTTP proxy URL, empty for direct connections. A saved proxy is
    /// preferred for every connection and falls back to a direct connection
    /// when unreachable. `TERMGRAM_PROXY` overrides it without fallback.
    pub proxy: String,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Settings")
            .field("automatic_update_checks", &self.automatic_update_checks)
            .field("release_channel", &self.release_channel)
            .field("download_behavior", &self.download_behavior)
            .field("show_message_ids", &self.show_message_ids)
            .field("active_account", &self.active_account)
            .field("account_count", &self.account_count)
            .field("proxy", &redact_proxy(&self.proxy))
            .finish()
    }
}

/// Format a proxy URL for diagnostics, hiding embedded credentials.
fn redact_proxy(proxy: &str) -> String {
    match url::Url::parse(proxy) {
        Ok(mut url) => {
            if !url.username().is_empty() {
                url.set_username("redacted").ok();
            }
            if url.password().is_some() {
                url.set_password(Some("redacted")).ok();
            }
            url.to_string()
        }
        Err(_) => "[invalid proxy URL]".to_owned(),
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            automatic_update_checks: true,
            release_channel: ReleaseChannel::Stable,
            download_behavior: DownloadBehavior::RevealOnActivation,
            show_message_ids: false,
            active_account: 1,
            account_count: 1,
            proxy: String::new(),
        }
    }
}

impl Settings {
    /// Return the platform-native path used for persisted preferences.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform config directory is unavailable.
    pub fn path() -> Result<PathBuf> {
        Ok(ProjectDirs::from("dev", "termgram", "Termgram")
            .context("could not determine the application config directory")?
            .config_dir()
            .join(SETTINGS_FILE_NAME))
    }

    /// Load persisted preferences, returning defaults when no file exists.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform path is unavailable or the settings
    /// file cannot be safely read or parsed.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path()?)
    }

    /// Load preferences from an explicit path. This is also useful to callers
    /// that isolate portable or test configurations.
    ///
    /// # Errors
    ///
    /// Returns an error for an unreadable, oversized, symbolic-link, or
    /// malformed settings file.
    pub fn load_from(path: &Path) -> Result<Self> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
            }
        };
        if metadata.file_type().is_symlink() {
            bail!("refusing to read settings through a symbolic link");
        }
        if !metadata.is_file() {
            bail!("settings path is not a regular file");
        }
        if metadata.len() > MAX_SETTINGS_BYTES {
            bail!("settings file is larger than {MAX_SETTINGS_BYTES} bytes");
        }
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        parse_settings(&text)
    }

    /// Atomically persist preferences in the platform-native config directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform path is unavailable or persistence
    /// fails.
    pub fn save(self) -> Result<()> {
        self.save_to(&Self::path()?)
    }

    /// Atomically persist preferences to an explicit path.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory is unsafe, the destination is not a
    /// regular file, or an atomic write cannot be completed.
    pub fn save_to(self, path: &Path) -> Result<()> {
        write_preferences(path, self.serialize().as_bytes())
    }

    fn serialize(self) -> String {
        format!(
            "version={SETTINGS_FORMAT_VERSION}\nautomatic_update_checks={}\nrelease_channel={}\ndownload_behavior={}\nshow_message_ids={}\nactive_account={}\naccount_count={}\nproxy={}\n",
            self.automatic_update_checks,
            self.release_channel.persisted(),
            self.download_behavior.persisted(),
            self.show_message_ids,
            self.active_account,
            self.account_count,
            self.proxy,
        )
    }
}

/// Read bounded JSON preferences, using defaults only when the file is absent.
///
/// # Errors
/// Returns read or format errors without replacing an existing file.
pub(crate) fn read_preferences<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(error) => return Err(error.into()),
    };
    if text.len() > 1024 * 1024 {
        bail!("preferences exceed 1 MiB");
    }
    Ok(serde_json::from_str(&text)?)
}

/// Shared atomic writer for managed preferences, with the same platform cleanup
/// and path protections as settings.conf.
pub(crate) fn write_preferences(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent_created = !parent.exists();
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let parent_metadata = fs::symlink_metadata(parent)
        .with_context(|| format!("failed to inspect {}", parent.display()))?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        bail!("settings directory must be a real directory");
    }
    protect_settings_directory(parent, parent_created)?;

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing to replace settings through a symbolic link");
        }
        Ok(metadata) if !metadata.is_file() => {
            bail!("settings path is not a regular file");
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(SETTINGS_FILE_NAME);
    let temporary = parent.join(format!(".{file_name}.tmp-{}-{nonce}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        file.write_all(contents)
            .with_context(|| format!("failed to write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temporary.display()))?;
        drop(file);
        replace_settings_file(&temporary, path, nonce)?;
        protect_settings_file(path)?;
        #[cfg(unix)]
        OpenOptions::new()
            .read(true)
            .open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("failed to sync {}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        drop(fs::remove_file(&temporary));
    }
    result
}

/// An ordered proxy route for Telegram connections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyRoute {
    /// SOCKS5 or HTTP CONNECT proxy URL, including a required port.
    pub url: String,
    /// Whether a failed proxy connection may fall back to a direct one.
    ///
    /// Saved proxies prefer the proxy but fall back to direct connections;
    /// the `TERMGRAM_PROXY` environment variable is strict and never falls
    /// back.
    pub fallback: bool,
}

/// The proxy resolved for this run, plus a warning for ignored invalid input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyResolution {
    pub proxy: Option<ProxyRoute>,
    pub warning: Option<String>,
}

/// Validate a SOCKS5 or HTTP CONNECT proxy URL the way the Telegram
/// connection accepts it.
///
/// # Errors
///
/// Returns a human-readable reason when the value is not a `socks5://` or
/// `http://` URL with a host and an explicit port.
pub fn validate_proxy(value: &str) -> Result<(), String> {
    let url = url::Url::parse(value).map_err(|error| error.to_string())?;
    if !matches!(url.scheme(), "socks5" | "http") {
        return Err("the scheme must be socks5 or http".to_owned());
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("a host is required".to_owned());
    }
    if url.port().is_none() {
        return Err("an explicit port is required".to_owned());
    }
    Ok(())
}

/// The effective `TERMGRAM_PROXY` value, when set to a non-empty string.
///
/// Reads the `.env` file first, matching the credential configuration.
#[must_use]
pub fn proxy_env_override() -> Option<String> {
    dotenvy::dotenv().ok();
    env::var("TERMGRAM_PROXY")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Resolve the proxy used for Telegram connections.
///
/// `TERMGRAM_PROXY` overrides the saved preference and is strict: when set,
/// connections never bypass the proxy with a direct fallback, and an invalid
/// value disables proxying entirely with a warning. The saved preference
/// prefers the proxy but falls back to direct connections when unreachable.
/// An invalid saved value is ignored with a warning.
#[must_use]
pub fn resolve_proxy(settings: &Settings) -> ProxyResolution {
    resolve_proxy_with(proxy_env_override(), settings)
}

fn resolve_proxy_with(env_override: Option<String>, settings: &Settings) -> ProxyResolution {
    if let Some(value) = env_override {
        return match validate_proxy(&value) {
            Ok(()) => ProxyResolution {
                proxy: Some(ProxyRoute {
                    url: value,
                    fallback: false,
                }),
                warning: None,
            },
            Err(error) => ProxyResolution {
                proxy: None,
                warning: Some(format!("Ignoring invalid TERMGRAM_PROXY: {error}")),
            },
        };
    }
    let value = settings.proxy.trim();
    if value.is_empty() {
        return ProxyResolution {
            proxy: None,
            warning: None,
        };
    }
    match validate_proxy(value) {
        Ok(()) => ProxyResolution {
            proxy: Some(ProxyRoute {
                url: value.to_owned(),
                fallback: true,
            }),
            warning: None,
        },
        Err(error) => ProxyResolution {
            proxy: None,
            warning: Some(format!("Ignoring the invalid saved proxy: {error}")),
        },
    }
}

fn parse_settings(text: &str) -> Result<Settings> {
    let mut settings = Settings::default();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .with_context(|| format!("invalid settings line {}", index + 1))?;
        match key.trim() {
            "version" if value.trim() == SETTINGS_FORMAT_VERSION => {}
            "version" => bail!("unsupported settings format version"),
            "automatic_update_checks" => {
                settings.automatic_update_checks = match value.trim() {
                    "true" => true,
                    "false" => false,
                    _ => bail!("automatic_update_checks must be true or false"),
                };
            }
            "release_channel" => {
                settings.release_channel = match value.trim() {
                    "stable" => ReleaseChannel::Stable,
                    "prerelease" => ReleaseChannel::Prerelease,
                    _ => bail!("release_channel must be stable or prerelease"),
                };
            }
            "download_behavior" => {
                settings.download_behavior = match value.trim() {
                    "cache_only" | "temp_only" => DownloadBehavior::CacheOnly,
                    "reveal_on_activation" => DownloadBehavior::RevealOnActivation,
                    _ => bail!("download_behavior has an unsupported value"),
                };
            }
            "show_message_ids" => {
                settings.show_message_ids = match value.trim() {
                    "true" => true,
                    "false" => false,
                    _ => bail!("show_message_ids must be true or false"),
                };
            }
            "active_account" => {
                settings.active_account = value
                    .trim()
                    .parse::<u8>()
                    .context("active_account must be a number")?;
            }
            "account_count" => {
                settings.account_count = value
                    .trim()
                    .parse::<u8>()
                    .context("account_count must be a number")?;
            }
            "proxy" => {
                value.trim().clone_into(&mut settings.proxy);
            }
            _ => {}
        }
    }
    if !(1..=MAX_ACCOUNTS).contains(&settings.account_count) {
        bail!("account_count must be between 1 and {MAX_ACCOUNTS}");
    }
    if settings.active_account == 0 || settings.active_account > settings.account_count {
        bail!("active_account must identify an existing account");
    }
    Ok(settings)
}

#[cfg(unix)]
fn protect_settings_directory(path: &Path, created: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if created {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to protect {}", path.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn protect_settings_directory(_path: &Path, _created: bool) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn protect_settings_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to protect {}", path.display()))
}

#[cfg(not(unix))]
fn protect_settings_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn replace_settings_file(temporary: &Path, path: &Path, _nonce: u128) -> Result<()> {
    fs::rename(temporary, path).with_context(|| format!("failed to replace {}", path.display()))
}

#[cfg(windows)]
fn replace_settings_file(temporary: &Path, path: &Path, nonce: u128) -> Result<()> {
    if !path.exists() {
        return fs::rename(temporary, path)
            .with_context(|| format!("failed to install {}", path.display()));
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(SETTINGS_FILE_NAME);
    let backup = path.with_file_name(format!(".{file_name}.bak-{}-{nonce}", std::process::id()));
    fs::rename(path, &backup)
        .with_context(|| format!("failed to stage existing {}", path.display()))?;
    if let Err(error) = fs::rename(temporary, path) {
        let rollback = fs::rename(&backup, path);
        return match rollback {
            Ok(()) => Err(error).with_context(|| format!("failed to replace {}", path.display())),
            Err(rollback_error) => bail!(
                "failed to replace {} ({error}) and restore backup {} ({rollback_error})",
                path.display(),
                backup.display()
            ),
        };
    }
    // The new file is already committed. A stale hidden backup is preferable
    // to reporting a false save failure after the requested value took effect.
    drop(fs::remove_file(&backup));
    Ok(())
}

#[derive(Clone, Eq, PartialEq)]
pub struct Config {
    pub api_id: i32,
    pub api_hash: String,
    pub session_path: PathBuf,
    /// Durable drafts and later user-authored state shared across account slots.
    pub state_path: PathBuf,
    /// Enable bounded background Ping observations for the Lua statusline.
    pub measure_latency: bool,
    /// Proxy used for every connection, resolved from the environment
    /// and saved preferences. `None` connects directly.
    pub proxy: Option<ProxyRoute>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Config")
            .field("api_id", &self.api_id)
            .field("api_hash", &"[redacted]")
            .field("session_path", &self.session_path)
            .field("state_path", &self.state_path)
            .field("measure_latency", &self.measure_latency)
            .field(
                "proxy",
                &self.proxy.as_ref().map(|route| redact_proxy(&route.url)),
            )
            .finish()
    }
}

impl Config {
    /// Load Telegram credentials and the optional session location.
    ///
    /// # Errors
    ///
    /// Returns an error when required environment variables are missing or
    /// invalid, or when the platform data directory cannot be determined.
    pub fn load() -> Result<Self> {
        dotenvy::dotenv().ok();

        let api_id = credential(
            "TELEGRAM_API_ID",
            env::var("TELEGRAM_API_ID"),
            EMBEDDED_API_ID,
        )?
        .parse::<i32>()
        .context("TELEGRAM_API_ID must be a number")?;
        if api_id <= 0 {
            bail!("TELEGRAM_API_ID must be positive");
        }

        let api_hash = credential(
            "TELEGRAM_API_HASH",
            env::var("TELEGRAM_API_HASH"),
            EMBEDDED_API_HASH,
        )?;
        if api_hash.trim().is_empty() || api_hash == "replace-me" {
            bail!("TELEGRAM_API_HASH is empty or still uses the example value");
        }

        let session_path = if let Some(path) = env::var_os("TERMGRAM_SESSION") {
            PathBuf::from(path)
        } else if let Some(path) = env::var_os("TUIGRAM_SESSION") {
            // Keep the old override working so upgrading does not silently
            // sign an existing user out.
            PathBuf::from(path)
        } else {
            let dirs = ProjectDirs::from("dev", "termgram", "Termgram")
                .context("could not determine the application data directory")?;
            let current = dirs.data_local_dir().join("termgram.session");
            let legacy = ProjectDirs::from("dev", "tuigram", "TUIGram")
                .map(|dirs| dirs.data_local_dir().join("tuigram.session"));
            choose_default_session_path(current, legacy)
        };

        let mut state_path = session_path.as_os_str().to_os_string();
        state_path.push(".state.sqlite3");
        Ok(Self {
            api_id,
            api_hash,
            state_path: PathBuf::from(state_path),
            session_path,
            measure_latency: false,
            proxy: None,
        })
    }

    /// Derive the private session database for a one-based account slot.
    /// Account one retains the historical path so existing users stay logged
    /// in; additional accounts live beside it in a dedicated subdirectory.
    ///
    /// # Errors
    ///
    /// Returns an error when `account` is outside the supported slot range or
    /// the configured session path has no usable file name.
    pub fn for_account(&self, account: u8) -> Result<Self> {
        if !(1..=MAX_ACCOUNTS).contains(&account) {
            bail!("account must be between 1 and {MAX_ACCOUNTS}");
        }
        if account == 1 {
            return Ok(self.clone());
        }
        let parent = self
            .session_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let file_name = self
            .session_path
            .file_name()
            .context("session path has no file name")?
            .to_string_lossy();
        let session_path = parent
            .join("accounts")
            .join(format!("{file_name}.account-{account}"));
        Ok(Self {
            api_id: self.api_id,
            api_hash: self.api_hash.clone(),
            state_path: self.state_path.clone(),
            session_path,
            measure_latency: self.measure_latency,
            proxy: self.proxy.clone(),
        })
    }

    /// Create the private directory that contains the Telegram session.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created or, on Unix, a
    /// newly-created directory cannot be restricted to the current user.
    pub fn prepare_session_dir(&self) -> Result<()> {
        let parent = self
            .session_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        #[cfg(unix)]
        let created = !parent.exists();
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create session directory {}", parent.display()))?;
        let parent_metadata = std::fs::symlink_metadata(parent)
            .with_context(|| format!("failed to inspect session directory {}", parent.display()))?;
        if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
            bail!("session directory must be a real directory");
        }
        match std::fs::symlink_metadata(&self.session_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("refusing to open a session through a symbolic link");
            }
            Ok(metadata) if !metadata.is_file() => {
                bail!("session path is not a regular file");
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", self.session_path.display()));
            }
        }
        #[cfg(unix)]
        if created {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("failed to protect {}", parent.display()))?;
        }
        #[cfg(unix)]
        {
            use std::fs::OpenOptions;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

            // Create the database privately before SQLite can create a WAL or
            // shared-memory sidecar using a permissive process umask.
            OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&self.session_path)
                .with_context(|| {
                    format!(
                        "failed to create session file {}",
                        self.session_path.display()
                    )
                })?;
            std::fs::set_permissions(&self.session_path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to protect {}", self.session_path.display()))?;
        }
        Ok(())
    }

    /// Restrict an existing session database to the current Unix user.
    ///
    /// # Errors
    ///
    /// Returns an error on Unix when the session permissions cannot be set.
    pub fn protect_session_file(&self) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if self.session_path.exists() {
                std::fs::set_permissions(
                    &self.session_path,
                    std::fs::Permissions::from_mode(0o600),
                )
                .with_context(|| format!("failed to protect {}", self.session_path.display()))?;
            }
            for suffix in ["-wal", "-shm"] {
                let mut sidecar = self.session_path.as_os_str().to_os_string();
                sidecar.push(suffix);
                let sidecar = PathBuf::from(sidecar);
                if sidecar.exists() {
                    std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o600))
                        .with_context(|| format!("failed to protect {}", sidecar.display()))?;
                }
            }
        }
        Ok(())
    }
}

fn credential(
    name: &str,
    runtime: std::result::Result<String, env::VarError>,
    embedded: Option<&str>,
) -> Result<String> {
    match runtime {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Ok(_) | Err(env::VarError::NotPresent) => embedded
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .with_context(|| format!("{name} is not set (copy .env.example to .env)")),
        Err(env::VarError::NotUnicode(_)) => bail!("{name} is not valid Unicode"),
    }
}

fn choose_default_session_path(current: PathBuf, legacy: Option<PathBuf>) -> PathBuf {
    match legacy {
        Some(path) if !current.exists() && path.exists() => path,
        _ => current,
    }
}

pub(crate) fn prepare_private_file(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            bail!("local storage must be a regular file")
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let metadata = std::fs::symlink_metadata(path)?;
                    if !metadata.is_file() || metadata.file_type().is_symlink() {
                        bail!("local storage must be a regular file");
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        Config, DownloadBehavior, MAX_ACCOUNTS, ReleaseChannel, Settings,
        choose_default_session_path, credential, resolve_proxy_with, validate_proxy,
    };

    fn temporary_settings_path(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "termgram-settings-{label}-{}-{nonce}",
                std::process::id()
            ))
            .join("settings.conf")
    }

    #[test]
    fn runtime_credentials_override_embedded_build_credentials() {
        let value = credential(
            "TELEGRAM_API_HASH",
            Ok("runtime".to_owned()),
            Some("embedded"),
        )
        .expect("runtime credential");
        assert_eq!(value, "runtime");
    }

    #[test]
    fn embedded_build_credentials_are_a_missing_runtime_fallback() {
        let value = credential(
            "TELEGRAM_API_HASH",
            Err(std::env::VarError::NotPresent),
            Some("embedded"),
        )
        .expect("embedded credential");
        assert_eq!(value, "embedded");

        assert!(
            credential(
                "TELEGRAM_API_HASH",
                Err(std::env::VarError::NotPresent),
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn settings_default_to_safe_essential_preferences() {
        let settings = Settings::default();
        assert!(settings.automatic_update_checks);
        assert_eq!(settings.release_channel, ReleaseChannel::Stable);
        assert_eq!(
            settings.download_behavior,
            DownloadBehavior::RevealOnActivation
        );
        assert!(!settings.show_message_ids);
        assert_eq!(settings.active_account, 1);
        assert_eq!(settings.account_count, 1);
    }

    #[test]
    fn settings_save_atomically_and_can_be_replaced() {
        let path = temporary_settings_path("replace");
        let first = Settings::default();
        first.clone().save_to(&path).expect("first save");
        assert_eq!(Settings::load_from(&path).expect("first load"), first);

        let second = Settings {
            automatic_update_checks: false,
            release_channel: ReleaseChannel::Prerelease,
            download_behavior: DownloadBehavior::CacheOnly,
            show_message_ids: true,
            active_account: 2,
            account_count: 3,
            proxy: "socks5://127.0.0.1:9050".to_owned(),
        };
        second.clone().save_to(&path).expect("replacement save");
        assert_eq!(Settings::load_from(&path).expect("second load"), second);
        assert_eq!(second.proxy, "socks5://127.0.0.1:9050");
        let directory = path.parent().expect("settings directory").to_path_buf();
        let leftovers = std::fs::read_dir(&directory)
            .expect("settings directory")
            .map(|entry| entry.expect("settings entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(leftovers, [std::ffi::OsString::from("settings.conf")]);

        std::fs::remove_file(&path).expect("remove settings");
        std::fs::remove_dir(directory).expect("remove settings directory");
    }

    #[test]
    fn settings_accept_unknown_future_keys_but_reject_bad_values() {
        let parsed = super::parse_settings(
            "version=1\nautomatic_update_checks=false\nfuture_option=value\n",
        )
        .expect("forward compatible settings");
        assert!(!parsed.automatic_update_checks);
        assert!(super::parse_settings("release_channel=nightly\n").is_err());
        assert!(super::parse_settings("automatic_update_checks=yes\n").is_err());
        assert!(super::parse_settings("show_message_ids=yes\n").is_err());
        assert!(super::parse_settings("account_count=0\n").is_err());
        assert!(super::parse_settings("account_count=2\nactive_account=3\n").is_err());
        assert!(super::parse_settings(&format!("account_count={}\n", MAX_ACCOUNTS + 1)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn settings_are_private_and_refuse_symbolic_links() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let path = temporary_settings_path("symlink");
        let directory = path.parent().expect("settings directory").to_path_buf();
        Settings::default().save_to(&path).expect("save settings");
        let mode = std::fs::metadata(&path)
            .expect("settings metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let directory_mode = std::fs::metadata(&directory)
            .expect("settings directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory_mode, 0o700);

        let target = path.with_file_name("target.conf");
        std::fs::write(&target, b"version=1\n").expect("write target");
        let link = path.with_file_name("linked.conf");
        symlink(&target, &link).expect("create symlink");
        assert!(Settings::load_from(&link).is_err());
        assert!(Settings::default().save_to(&link).is_err());
        assert_eq!(
            std::fs::read_to_string(&target).expect("target unchanged"),
            "version=1\n"
        );

        std::fs::remove_file(link).expect("remove symlink");
        std::fs::remove_file(target).expect("remove target");
        std::fs::remove_file(path).expect("remove settings");
        std::fs::remove_dir(directory).expect("remove settings directory");
    }

    #[test]
    fn debug_output_redacts_the_api_hash() {
        let config = Config {
            state_path: PathBuf::from("unused-state"),
            measure_latency: false,
            api_id: 42,
            api_hash: "super-secret".to_owned(),
            session_path: PathBuf::from("session.db"),
            proxy: None,
        };
        let debug = format!("{config:?}");
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains("super-secret"));
    }

    #[test]
    fn additional_accounts_use_distinct_sibling_session_files() {
        let base = Config {
            state_path: PathBuf::from("unused-state"),
            measure_latency: false,
            api_id: 42,
            api_hash: "secret".to_owned(),
            session_path: PathBuf::from("state/custom.session"),
            proxy: None,
        };
        assert_eq!(
            base.for_account(1).expect("first account").session_path,
            PathBuf::from("state/custom.session")
        );
        assert_eq!(
            base.for_account(2).expect("second account").session_path,
            PathBuf::from("state/accounts/custom.session.account-2")
        );
        assert!(base.for_account(0).is_err());
        assert!(base.for_account(MAX_ACCOUNTS + 1).is_err());
    }

    #[test]
    fn reuses_an_existing_legacy_session_without_overriding_a_new_one() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "termgram-session-migration-test-{}-{nonce}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&root).expect("temporary directory");
        let current = root.join("termgram.session");
        let legacy = root.join("tuigram.session");
        std::fs::File::create(&legacy).expect("legacy session");

        assert_eq!(
            choose_default_session_path(current.clone(), Some(legacy.clone())),
            legacy
        );

        std::fs::File::create(&current).expect("current session");
        assert_eq!(
            choose_default_session_path(current.clone(), Some(legacy.clone())),
            current
        );

        std::fs::remove_file(current).expect("remove current session");
        std::fs::remove_file(legacy).expect("remove legacy session");
        std::fs::remove_dir(root).expect("remove test directory");
    }

    #[cfg(unix)]
    #[test]
    fn prepares_a_private_session_file_in_an_existing_directory() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = std::env::temp_dir().join(format!(
            "termgram-config-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("thread")
        ));
        std::fs::create_dir_all(&root).expect("temporary directory");
        let session_path = root.join("session.db");
        let config = Config {
            state_path: PathBuf::from("unused-state"),
            measure_latency: false,
            api_id: 42,
            api_hash: "secret".to_owned(),
            session_path: session_path.clone(),
            proxy: None,
        };

        config.prepare_session_dir().expect("prepare session");
        let mode = std::fs::metadata(&session_path)
            .expect("session metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        let additional = config.for_account(2).expect("second account path");
        additional
            .prepare_session_dir()
            .expect("prepare second account");
        let additional_mode = std::fs::metadata(&additional.session_path)
            .expect("second session metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(additional_mode, 0o600);
        let accounts_directory = additional
            .session_path
            .parent()
            .expect("accounts directory");
        let directory_mode = std::fs::metadata(accounts_directory)
            .expect("accounts directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory_mode, 0o700);

        let symlink_target = root.join("symlink-target.session");
        std::fs::File::create(&symlink_target).expect("create symlink target");
        let symlink_path = root.join("symlink.session");
        symlink(&symlink_target, &symlink_path).expect("create session symlink");
        let unsafe_config = Config {
            state_path: PathBuf::from("unused-state"),
            session_path: symlink_path.clone(),
            ..config.clone()
        };
        assert!(unsafe_config.prepare_session_dir().is_err());

        std::fs::remove_file(symlink_path).expect("remove session symlink");
        std::fs::remove_file(symlink_target).expect("remove symlink target");
        std::fs::remove_file(&additional.session_path).expect("remove second session");
        std::fs::remove_dir(accounts_directory).expect("remove accounts directory");
        std::fs::remove_file(session_path).expect("remove test session");
        std::fs::remove_dir(root).expect("remove test directory");
    }

    #[test]
    fn proxy_validation_requires_supported_scheme_host_and_port() {
        assert!(validate_proxy("socks5://127.0.0.1:9050").is_ok());
        assert!(validate_proxy("socks5://user:pass@example.com:5678").is_ok());
        assert!(validate_proxy("http://127.0.0.1:3128").is_ok());
        assert!(validate_proxy("http://user:pass@example.com:8080").is_ok());
        assert!(validate_proxy("https://127.0.0.1:3128").is_err());
        assert!(validate_proxy("socks5://127.0.0.1").is_err());
        assert!(validate_proxy("not a url").is_err());
    }

    #[test]
    fn saved_proxy_prefers_the_proxy_but_falls_back_to_direct() {
        let settings = Settings {
            proxy: "socks5://127.0.0.1:9050".to_owned(),
            ..Settings::default()
        };
        let resolved = resolve_proxy_with(None, &settings);
        let route = resolved.proxy.expect("saved proxy");
        assert_eq!(route.url, "socks5://127.0.0.1:9050");
        assert!(route.fallback);
        assert!(resolved.warning.is_none());
    }

    #[test]
    fn empty_preferences_connect_directly() {
        let resolved = resolve_proxy_with(None, &Settings::default());
        assert!(resolved.proxy.is_none());
        assert!(resolved.warning.is_none());
    }

    #[test]
    fn environment_proxy_is_strict_and_overrides_the_saved_preference() {
        let saved = Settings {
            proxy: "socks5://127.0.0.1:9050".to_owned(),
            ..Settings::default()
        };
        let resolved = resolve_proxy_with(Some("socks5://10.0.0.1:1080".to_owned()), &saved);
        let route = resolved.proxy.expect("environment proxy");
        assert_eq!(route.url, "socks5://10.0.0.1:1080");
        assert!(!route.fallback);
        assert!(resolved.warning.is_none());
    }

    #[test]
    fn invalid_environment_proxy_is_ignored_with_a_warning_and_never_falls_back() {
        let saved = Settings {
            proxy: "socks5://127.0.0.1:9050".to_owned(),
            ..Settings::default()
        };
        let resolved = resolve_proxy_with(Some("https://10.0.0.1:1080".to_owned()), &saved);
        assert!(resolved.proxy.is_none());
        assert!(resolved.warning.is_some());
    }

    #[test]
    fn invalid_saved_proxy_is_ignored_with_a_warning() {
        let settings = Settings {
            proxy: "nonsense".to_owned(),
            ..Settings::default()
        };
        let resolved = resolve_proxy_with(None, &settings);
        assert!(resolved.proxy.is_none());
        assert!(resolved.warning.is_some());
    }

    #[test]
    fn debug_output_redacts_proxy_credentials() {
        let settings = Settings {
            proxy: "socks5://user:secret@127.0.0.1:9050".to_owned(),
            ..Settings::default()
        };
        let debug = format!("{settings:?}");
        assert!(debug.contains("redacted"));
        assert!(!debug.contains("secret"));
        assert!(debug.contains("socks5://"));
    }
}
