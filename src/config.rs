use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use directories::ProjectDirs;

const EMBEDDED_API_ID: Option<&str> = option_env!("TERMGRAM_EMBEDDED_API_ID");
const EMBEDDED_API_HASH: Option<&str> = option_env!("TERMGRAM_EMBEDDED_API_HASH");
const SETTINGS_FILE_NAME: &str = "settings.conf";
const SETTINGS_FORMAT_VERSION: &str = "1";
const MAX_SETTINGS_BYTES: u64 = 16 * 1024;

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
    /// Download to Termgram's private temporary directory, but never ask the
    /// operating system to reveal the file.
    TempOnly,
    /// A second explicit activation reveals the containing folder or selects
    /// the file; downloaded content is never executed directly.
    #[default]
    RevealOnActivation,
}

impl DownloadBehavior {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::TempOnly => "Temp only",
            Self::RevealOnActivation => "Reveal on activation",
        }
    }

    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::TempOnly => Self::RevealOnActivation,
            Self::RevealOnActivation => Self::TempOnly,
        }
    }

    const fn persisted(self) -> &'static str {
        match self {
            Self::TempOnly => "temp_only",
            Self::RevealOnActivation => "reveal_on_activation",
        }
    }
}

/// Small, non-sensitive preferences stored in Termgram's platform config
/// directory. Telegram credentials and sessions are deliberately excluded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Settings {
    pub automatic_update_checks: bool,
    pub release_channel: ReleaseChannel,
    pub download_behavior: DownloadBehavior,
    /// Reserve a compact, right-aligned message identifier column in the
    /// conversation pane. Reply headers always show their target identifier
    /// regardless of this preference.
    pub show_message_ids: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            automatic_update_checks: true,
            release_channel: ReleaseChannel::Stable,
            download_behavior: DownloadBehavior::RevealOnActivation,
            show_message_ids: false,
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
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent_created = !parent.exists();
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
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
            file.write_all(self.serialize().as_bytes())
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

    fn serialize(self) -> String {
        format!(
            "version={SETTINGS_FORMAT_VERSION}\nautomatic_update_checks={}\nrelease_channel={}\ndownload_behavior={}\nshow_message_ids={}\n",
            self.automatic_update_checks,
            self.release_channel.persisted(),
            self.download_behavior.persisted(),
            self.show_message_ids,
        )
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
                    "temp_only" => DownloadBehavior::TempOnly,
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
            _ => {}
        }
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
}

impl std::fmt::Debug for Config {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Config")
            .field("api_id", &self.api_id)
            .field("api_hash", &"[redacted]")
            .field("session_path", &self.session_path)
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

        Ok(Self {
            api_id,
            api_hash,
            session_path,
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        choose_default_session_path, credential, Config, DownloadBehavior, ReleaseChannel, Settings,
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

        assert!(credential(
            "TELEGRAM_API_HASH",
            Err(std::env::VarError::NotPresent),
            None,
        )
        .is_err());
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
    }

    #[test]
    fn settings_save_atomically_and_can_be_replaced() {
        let path = temporary_settings_path("replace");
        let first = Settings::default();
        first.save_to(&path).expect("first save");
        assert_eq!(Settings::load_from(&path).expect("first load"), first);

        let second = Settings {
            automatic_update_checks: false,
            release_channel: ReleaseChannel::Prerelease,
            download_behavior: DownloadBehavior::TempOnly,
            show_message_ids: true,
        };
        second.save_to(&path).expect("replacement save");
        assert_eq!(Settings::load_from(&path).expect("second load"), second);
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
    }

    #[cfg(unix)]
    #[test]
    fn settings_are_private_and_refuse_symbolic_links() {
        use std::os::unix::fs::{symlink, PermissionsExt};

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
            api_id: 42,
            api_hash: "super-secret".to_owned(),
            session_path: PathBuf::from("session.db"),
        };
        let debug = format!("{config:?}");
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains("super-secret"));
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
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "termgram-config-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("thread")
        ));
        std::fs::create_dir_all(&root).expect("temporary directory");
        let session_path = root.join("session.db");
        let config = Config {
            api_id: 42,
            api_hash: "secret".to_owned(),
            session_path: session_path.clone(),
        };

        config.prepare_session_dir().expect("prepare session");
        let mode = std::fs::metadata(&session_path)
            .expect("session metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        std::fs::remove_file(session_path).expect("remove test session");
        std::fs::remove_dir(root).expect("remove test directory");
    }
}
