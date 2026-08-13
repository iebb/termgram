//! Check for and install checksum-verified Termgram releases.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::ffi::OsString;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::config::{ReleaseChannel, Settings};

const RELEASES_URL: &str = "https://api.github.com/repos/iebb/termgram/releases?per_page=100";
const LATEST_URL: &str = "https://api.github.com/repos/iebb/termgram/releases/latest";
const RELEASE_PAGE: &str = "https://github.com/iebb/termgram/releases/tag";
const DOWNLOAD_ROOT: &str = "https://github.com/iebb/termgram/releases/download";
const USER_AGENT: &str = "termgram-updater";
const MAX_METADATA_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CHECKSUM_BYTES: u64 = 64 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
#[cfg(windows)]
const FAILED_REPLACEMENT_WARNING: &str =
    "A previous Termgram update could not replace tg; close other tg processes, then run `tg update` again";

static AUTOMATIC_CHECKS: Mutex<AutomaticCheckThrottle> = Mutex::new(AutomaticCheckThrottle::new());

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateStatus {
    UpToDate,
    Available { version: String, url: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateOutcome {
    UpToDate,
    Installed {
        version: String,
    },
    /// Windows cannot replace a running executable. A small detached helper
    /// completes the atomic replacement as soon as this process exits.
    Staged {
        version: String,
        path: PathBuf,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Release {
    version: String,
    height: u64,
    prerelease: bool,
    draft: bool,
}

#[derive(Clone, Copy)]
struct Platform {
    asset_label: &'static str,
    archive_extension: &'static str,
    binary_name: &'static str,
    compressed_tar: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AutomaticCheckState {
    Idle,
    InFlight,
    Failed,
}

struct AutomaticCheckThrottle {
    states: [AutomaticCheckState; 2],
}

impl AutomaticCheckThrottle {
    const fn new() -> Self {
        Self {
            states: [AutomaticCheckState::Idle; 2],
        }
    }

    fn begin(&mut self, channel: ReleaseChannel) -> bool {
        let state = &mut self.states[channel_index(channel)];
        if *state != AutomaticCheckState::Idle {
            return false;
        }
        *state = AutomaticCheckState::InFlight;
        true
    }

    fn finish(&mut self, channel: ReleaseChannel, succeeded: bool) {
        self.states[channel_index(channel)] = if succeeded {
            AutomaticCheckState::Idle
        } else {
            AutomaticCheckState::Failed
        };
    }
}

/// Check GitHub for a newer release in the selected channel.
///
/// # Errors
///
/// Returns an error when `curl` is unavailable, GitHub cannot be reached, or
/// GitHub returns invalid release metadata.
pub fn check(channel: ReleaseChannel) -> Result<UpdateStatus> {
    let temporary = TemporaryDirectory::create("check")?;
    let metadata = temporary.path().join("releases.json");
    let url = match channel {
        ReleaseChannel::Stable => LATEST_URL,
        ReleaseChannel::Prerelease => RELEASES_URL,
    };
    download(url, &metadata, MAX_METADATA_BYTES)?;
    let releases = parse_release_file(&metadata)?;
    select_update(&releases, channel, crate::VERSION)
}

/// Perform an automatic availability check at most once per 24 hours.
///
/// A completed GitHub response is recorded for 24 hours. Failed checks are
/// suppressed only for the rest of this process, so an offline launch cannot
/// create a persistent blind spot. Changing release channel makes the next
/// check immediately due.
///
/// # Errors
///
/// Returns an error when the marker cannot be written or the due check fails.
pub fn check_if_due(channel: ReleaseChannel) -> Result<Option<UpdateStatus>> {
    let marker = update_marker_path()?;
    let now = unix_seconds();
    if !check_is_due(&marker, channel, now)? {
        return Ok(None);
    }
    if !begin_automatic_check(channel) {
        return Ok(None);
    }
    let result = check(channel).and_then(|status| {
        record_check_success(&marker, channel, now)?;
        Ok(Some(status))
    });
    finish_automatic_check(channel, result.is_ok());
    result
}

fn channel_index(channel: ReleaseChannel) -> usize {
    match channel {
        ReleaseChannel::Stable => 0,
        ReleaseChannel::Prerelease => 1,
    }
}

fn begin_automatic_check(channel: ReleaseChannel) -> bool {
    AUTOMATIC_CHECKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .begin(channel)
}

fn finish_automatic_check(channel: ReleaseChannel, succeeded: bool) {
    AUTOMATIC_CHECKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .finish(channel, succeeded);
}

/// Return a fixed, terminal-safe warning when a Windows replacement failed.
///
/// # Errors
///
/// Returns an error if the state marker is malformed or unsafe to inspect.
#[cfg(windows)]
pub fn pending_replacement_warning() -> Result<Option<&'static str>> {
    let target = std::env::current_exe().context("could not locate the running tg executable")?;
    let marker = replacement_failure_marker_path(&target)?;
    match fs::symlink_metadata(&marker) {
        Ok(metadata)
            if metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 64 =>
        {
            Ok(Some(FAILED_REPLACEMENT_WARNING))
        }
        Ok(_) => bail!("refusing to trust an unsafe update failure marker"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("failed to inspect the update failure marker"),
    }
}

/// Non-Windows executables are replaced synchronously and never need a
/// handoff marker.
///
/// # Errors
///
/// This platform implementation does not return an error.
#[cfg(not(windows))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the Windows implementation validates a persisted marker"
)]
pub const fn pending_replacement_warning() -> Result<Option<&'static str>> {
    Ok(None)
}

/// Download, verify, and install the newest release in the selected channel.
///
/// The release archive is integrity-checked against its exact entry in the
/// release's `SHA256SUMS`, its shape is checked before extraction, and the new
/// executable is staged beside the current executable before atomic replace.
///
/// # Errors
///
/// Returns an error when download, verification, extraction, or replacement
/// fails. The existing executable is left untouched on failure.
pub fn run(channel: ReleaseChannel) -> Result<UpdateOutcome> {
    let UpdateStatus::Available { version, .. } = check(channel)? else {
        #[cfg(windows)]
        clear_current_replacement_marker()?;
        return Ok(UpdateOutcome::UpToDate);
    };
    let platform = current_platform()?;
    let tag = format!("v{version}");
    let asset = format!(
        "termgram-{version}-{}.{}",
        platform.asset_label, platform.archive_extension
    );
    let temporary = TemporaryDirectory::create("install")?;
    let checksum_path = temporary.path().join("SHA256SUMS");
    let archive_path = temporary.path().join(&asset);
    let release_root = format!("{DOWNLOAD_ROOT}/{tag}");
    download(
        &format!("{release_root}/SHA256SUMS"),
        &checksum_path,
        MAX_CHECKSUM_BYTES,
    )?;
    download(
        &format!("{release_root}/{asset}"),
        &archive_path,
        MAX_ARCHIVE_BYTES,
    )?;
    verify_archive(&archive_path, &checksum_path, &asset)?;

    let extracted = temporary.path().join("extracted");
    fs::create_dir(&extracted)
        .with_context(|| format!("failed to create {}", extracted.display()))?;
    extract_exact_binary(&archive_path, &extracted, platform)?;
    let binary = extracted.join(platform.binary_name);
    let target = std::env::current_exe().context("could not locate the running tg executable")?;
    validate_update_target(&target)?;
    let staged = stage_binary(&binary, &target)?;

    #[cfg(windows)]
    {
        spawn_windows_replacer(&staged, &target)?;
        return Ok(UpdateOutcome::Staged {
            version,
            path: staged,
        });
    }

    #[cfg(not(windows))]
    {
        if let Err(error) = fs::rename(&staged, &target) {
            drop(fs::remove_file(&staged));
            return Err(error)
                .with_context(|| format!("failed to atomically replace {}", target.display()));
        }
        // The replacement is already committed at this point. Directory sync
        // is best-effort because some filesystems do not support it, and
        // reporting a failed update after replacement would be misleading.
        drop(sync_parent(&target));
        Ok(UpdateOutcome::Installed { version })
    }
}

fn parse_release_file(path: &Path) -> Result<Vec<Release>> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.len() > MAX_METADATA_BYTES {
        bail!("GitHub release metadata exceeded the size limit");
    }
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    JsonCursor::new(&bytes).parse_releases()
}

fn select_update(
    releases: &[Release],
    channel: ReleaseChannel,
    current: &str,
) -> Result<UpdateStatus> {
    let current_height = parse_version(current)
        .with_context(|| format!("current version {current:?} is not on the 0.1.Z line"))?;
    let newest = releases
        .iter()
        .filter(|release| {
            !release.draft && (channel == ReleaseChannel::Prerelease || !release.prerelease)
        })
        .max_by_key(|release| release.height)
        .context("no matching Termgram release is available")?;
    if newest.height <= current_height {
        return Ok(UpdateStatus::UpToDate);
    }
    Ok(UpdateStatus::Available {
        version: newest.version.clone(),
        url: format!("{RELEASE_PAGE}/v{}", newest.version),
    })
}

fn parse_version(version: &str) -> Option<u64> {
    let height = version.strip_prefix("0.1.")?;
    if height.is_empty()
        || !height.bytes().all(|byte| byte.is_ascii_digit())
        || (height.len() > 1 && height.starts_with('0'))
    {
        return None;
    }
    height.parse().ok()
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "unsupported targets return an error"
)]
fn current_platform() -> Result<Platform> {
    Ok(Platform {
        asset_label: "linux",
        archive_extension: "tar.gz",
        binary_name: "tg",
        compressed_tar: true,
    })
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "unsupported targets return an error"
)]
fn current_platform() -> Result<Platform> {
    Ok(Platform {
        asset_label: "linux-aarch64",
        archive_extension: "tar.gz",
        binary_name: "tg",
        compressed_tar: true,
    })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "unsupported targets return an error"
)]
fn current_platform() -> Result<Platform> {
    Ok(Platform {
        asset_label: "macos",
        archive_extension: "tar.gz",
        binary_name: "tg",
        compressed_tar: true,
    })
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "unsupported targets return an error"
)]
fn current_platform() -> Result<Platform> {
    Ok(Platform {
        asset_label: "macos-x86_64",
        archive_extension: "tar.gz",
        binary_name: "tg",
        compressed_tar: true,
    })
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "unsupported targets return an error"
)]
fn current_platform() -> Result<Platform> {
    Ok(Platform {
        asset_label: "windows",
        archive_extension: "zip",
        binary_name: "tg.exe",
        compressed_tar: false,
    })
}

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "unsupported targets return an error"
)]
fn current_platform() -> Result<Platform> {
    Ok(Platform {
        asset_label: "windows-aarch64",
        archive_extension: "zip",
        binary_name: "tg.exe",
        compressed_tar: false,
    })
}

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "aarch64")
)))]
fn current_platform() -> Result<Platform> {
    bail!(
        "no Termgram release is published for {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn download(url: &str, destination: &Path, max_bytes: u64) -> Result<()> {
    let max_bytes_argument = max_bytes.to_string();
    let max_time = if max_bytes == MAX_ARCHIVE_BYTES {
        "300"
    } else {
        "30"
    };
    let status = curl_command()?
        .args([
            "--disable",
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "--connect-timeout",
            "10",
            "--max-time",
            max_time,
            "--retry",
            "2",
            "--max-filesize",
            &max_bytes_argument,
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "X-GitHub-Api-Version: 2022-11-28",
            "--user-agent",
            USER_AGENT,
            "--output",
        ])
        .arg(destination)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| "failed to run curl; install curl to use Termgram updates")?;
    if !status.success() {
        bail!("curl could not download {url}");
    }
    let size = fs::metadata(destination)
        .with_context(|| format!("download did not create {}", destination.display()))?
        .len();
    if size > max_bytes {
        bail!("download from {url} exceeded the size limit");
    }
    Ok(())
}

#[cfg(not(windows))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "Windows validates an absolute system path"
)]
fn curl_command() -> Result<Command> {
    Ok(Command::new("curl"))
}

#[cfg(windows)]
fn curl_command() -> Result<Command> {
    windows_system_command("curl.exe")
}

fn verify_archive(archive: &Path, checksums: &Path, asset: &str) -> Result<()> {
    let checksum_text = fs::read_to_string(checksums)
        .with_context(|| format!("failed to read {}", checksums.display()))?;
    let expected = checksum_for_asset(&checksum_text, asset)?;
    let mut file =
        File::open(archive).with_context(|| format!("failed to read {}", archive.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to hash {}", archive.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual = digest
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            use std::fmt::Write as _;
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        });
    if actual != expected {
        bail!("checksum verification failed for {asset}");
    }
    Ok(())
}

fn checksum_for_asset(contents: &str, asset: &str) -> Result<String> {
    let mut matches = Vec::new();
    for line in contents.lines() {
        let Some((checksum, name)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let name = name
            .trim_start()
            .strip_prefix('*')
            .unwrap_or(name.trim_start());
        if name == asset {
            if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!("invalid checksum for {asset}");
            }
            matches.push(checksum.to_ascii_lowercase());
        }
    }
    if matches.len() != 1 {
        bail!("SHA256SUMS must contain exactly one entry for {asset}");
    }
    Ok(matches.pop().expect("one checksum"))
}

#[allow(
    clippy::too_many_lines,
    reason = "archive validation and bounded extraction form one security boundary"
)]
fn extract_exact_binary(archive: &Path, destination: &Path, platform: Platform) -> Result<()> {
    let list_flag = if platform.compressed_tar {
        "-tzf"
    } else {
        "-tf"
    };
    let extract_flag = if platform.compressed_tar {
        "-xzf"
    } else {
        "-xf"
    };
    let mut listing_command = tar_command()?;
    listing_command
        .arg(list_flag)
        .arg(archive)
        .env_remove("TAR_OPTIONS");
    let (listing_status, listing) = bounded_output(&mut listing_command, 4 * 1024)
        .context("failed to run tar; install tar to use Termgram updates")?;
    if !listing_status.success() {
        bail!("could not inspect the downloaded release archive");
    }
    let members = String::from_utf8(listing).context("archive listing was not UTF-8")?;
    let members = members
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    if members != [platform.binary_name] {
        bail!(
            "release archive must contain only a root-level {}",
            platform.binary_name
        );
    }
    let verbose_flag = if platform.compressed_tar {
        "-tvzf"
    } else {
        "-tvf"
    };
    let mut details_command = tar_command()?;
    details_command
        .arg(verbose_flag)
        .arg(archive)
        .env_remove("TAR_OPTIONS");
    let (details_status, details) = bounded_output(&mut details_command, 8 * 1024)
        .context("failed to inspect the release archive member")?;
    if !details_status.success()
        || details.first() != Some(&b'-')
        || details.split(|&byte| byte == b'\n').count() != 2
    {
        bail!("release archive member is not one regular file");
    }

    let binary = destination.join(platform.binary_name);
    let mut output_options = OpenOptions::new();
    output_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        output_options.mode(0o600);
    }
    let mut output = output_options
        .open(&binary)
        .with_context(|| format!("failed to create {}", binary.display()))?;
    let mut child = tar_command()?
        .arg(extract_flag)
        .arg(archive)
        .arg("-O")
        .arg(platform.binary_name)
        .env_remove("TAR_OPTIONS")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to run tar while extracting the update")?;
    let result = (|| -> Result<()> {
        let mut source = child.stdout.take().context("tar did not provide output")?;
        let mut buffer = [0_u8; 8 * 1024];
        let mut written = 0_u64;
        loop {
            let read = source
                .read(&mut buffer)
                .context("failed to extract the update")?;
            if read == 0 {
                break;
            }
            written = written.saturating_add(read as u64);
            if written > MAX_BINARY_BYTES {
                drop(child.kill());
                bail!("release binary exceeded the size limit");
            }
            output
                .write_all(&buffer[..read])
                .context("failed to write the extracted update")?;
        }
        if !child.wait().context("failed to wait for tar")?.success() {
            bail!("could not extract the downloaded release archive");
        }
        output
            .sync_all()
            .context("failed to sync the extracted update")?;
        Ok(())
    })();
    if let Err(error) = result {
        drop(child.kill());
        drop(child.wait());
        drop(output);
        drop(fs::remove_file(&binary));
        return Err(error);
    }
    if fs::metadata(&binary)?.len() == 0 {
        drop(fs::remove_file(&binary));
        bail!("release binary is empty");
    }
    let metadata = fs::symlink_metadata(&binary)
        .with_context(|| format!("release did not contain {}", binary.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        drop(fs::remove_file(&binary));
        bail!("release binary is not a regular file");
    }
    Ok(())
}

#[cfg(not(windows))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "Windows validates an absolute system path"
)]
fn tar_command() -> Result<Command> {
    Ok(Command::new("tar"))
}

#[cfg(windows)]
fn tar_command() -> Result<Command> {
    windows_system_command("tar.exe")
}

#[cfg(windows)]
fn windows_system_command(executable: &str) -> Result<Command> {
    let system_root = std::env::var_os("SystemRoot").context("SystemRoot is not set")?;
    let root = PathBuf::from(system_root);
    if !root.is_absolute() {
        bail!("SystemRoot is not absolute");
    }
    let path = root.join("System32").join(executable);
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!(
            "trusted system executable is unavailable: {}",
            path.display()
        );
    }
    Ok(Command::new(path))
}

fn bounded_output(command: &mut Command, max_bytes: usize) -> Result<(ExitStatus, Vec<u8>)> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start command")?;
    let result = (|| -> Result<Vec<u8>> {
        let mut source = child
            .stdout
            .take()
            .context("command did not provide output")?;
        let mut output = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = source
                .read(&mut buffer)
                .context("failed to read command output")?;
            if read == 0 {
                return Ok(output);
            }
            if output.len().saturating_add(read) > max_bytes {
                drop(child.kill());
                bail!("command output exceeded the size limit");
            }
            output.extend_from_slice(&buffer[..read]);
        }
    })();
    let status = child.wait().context("failed to wait for command")?;
    result.map(|output| (status, output))
}

fn validate_update_target(target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(target)
        .with_context(|| format!("failed to inspect {}", target.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("refusing to replace a non-regular executable");
    }
    let parent = target
        .parent()
        .context("running executable has no parent directory")?;
    let parent_metadata = fs::symlink_metadata(parent)
        .with_context(|| format!("failed to inspect {}", parent.display()))?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        bail!("executable directory is not a real directory");
    }
    Ok(())
}

fn stage_binary(source: &Path, target: &Path) -> Result<PathBuf> {
    let parent = target
        .parent()
        .context("running executable has no parent directory")?;
    let staged = unique_child(parent, ".tg-update", executable_suffix())?;
    let result = (|| -> Result<()> {
        let mut input =
            File::open(source).with_context(|| format!("failed to read {}", source.display()))?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o755);
        }
        let mut output = options
            .open(&staged)
            .with_context(|| format!("cannot write updates beside {}", target.display()))?;
        std::io::copy(&mut input, &mut output)
            .with_context(|| format!("failed to stage {}", staged.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            output
                .set_permissions(fs::Permissions::from_mode(0o755))
                .with_context(|| format!("failed to protect {}", staged.display()))?;
        }
        output
            .sync_all()
            .with_context(|| format!("failed to sync {}", staged.display()))?;
        Ok(())
    })();
    if let Err(error) = result {
        drop(fs::remove_file(&staged));
        return Err(error);
    }
    Ok(staged)
}

const fn executable_suffix() -> &'static str {
    if cfg!(windows) {
        ".exe"
    } else {
        ""
    }
}

fn unique_child(parent: &Path, prefix: &str, suffix: &str) -> Result<PathBuf> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0_u8..32 {
        let path = parent.join(format!(
            "{prefix}-{}-{nonce}-{attempt}{suffix}",
            std::process::id()
        ));
        if !path.exists() {
            return Ok(path);
        }
    }
    bail!("could not allocate a unique update path")
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    let parent = path.parent().context("updated executable has no parent")?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("failed to sync {}", parent.display()))
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn spawn_windows_replacer(staged: &Path, target: &Path) -> Result<()> {
    let failure_marker = replacement_failure_marker_path(target)?;
    remove_safe_failure_marker(&failure_marker)?;
    let backup = unique_child(
        target
            .parent()
            .context("running executable has no parent")?,
        ".tg-backup",
        ".exe",
    )?;
    let script = r#"
$ErrorActionPreference = 'Stop'
Wait-Process -Id ([int]$env:TERMGRAM_UPDATE_PID) -ErrorAction SilentlyContinue
for ($attempt = 0; $attempt -lt 60; $attempt++) {
  try {
    [IO.File]::Replace($env:TERMGRAM_UPDATE_STAGE, $env:TERMGRAM_UPDATE_TARGET, $env:TERMGRAM_UPDATE_BACKUP, $true)
    try {
      if ([IO.File]::Exists($env:TERMGRAM_UPDATE_BACKUP)) { [IO.File]::Delete($env:TERMGRAM_UPDATE_BACKUP) }
    } catch {}
    exit 0
  } catch {
    Start-Sleep -Milliseconds 500
  }
}
try {
  if (-not [IO.File]::Exists($env:TERMGRAM_UPDATE_TARGET) -and [IO.File]::Exists($env:TERMGRAM_UPDATE_BACKUP)) {
    [IO.File]::Move($env:TERMGRAM_UPDATE_BACKUP, $env:TERMGRAM_UPDATE_TARGET)
  }
} catch {}
try {
  if ([IO.File]::Exists($env:TERMGRAM_UPDATE_TARGET) -and [IO.File]::Exists($env:TERMGRAM_UPDATE_BACKUP)) {
    [IO.File]::Delete($env:TERMGRAM_UPDATE_BACKUP)
  }
} catch {}
try {
  $stream = [IO.File]::Open($env:TERMGRAM_UPDATE_FAILURE, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
  try {
    $bytes = [Text.Encoding]::UTF8.GetBytes("failed`n")
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Flush($true)
  } finally {
    $stream.Dispose()
  }
} catch {}
try {
  if ([IO.File]::Exists($env:TERMGRAM_UPDATE_STAGE)) { [IO.File]::Delete($env:TERMGRAM_UPDATE_STAGE) }
} catch {}
exit 1
"#;
    let system_root = std::env::var_os("SystemRoot").context("SystemRoot is not set")?;
    let root = PathBuf::from(system_root);
    if !root.is_absolute() {
        bail!("SystemRoot is not absolute");
    }
    let powershell = root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let metadata = fs::symlink_metadata(&powershell)
        .with_context(|| format!("failed to inspect {}", powershell.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("trusted PowerShell executable is unavailable");
    }
    let spawned = Command::new(powershell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .env("TERMGRAM_UPDATE_PID", std::process::id().to_string())
        .env("TERMGRAM_UPDATE_STAGE", staged)
        .env("TERMGRAM_UPDATE_TARGET", target)
        .env("TERMGRAM_UPDATE_BACKUP", backup)
        .env("TERMGRAM_UPDATE_FAILURE", failure_marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(error) = spawned {
        drop(fs::remove_file(staged));
        return Err(error).context("failed to start the Windows update helper");
    }
    Ok(())
}

#[cfg(windows)]
fn replacement_failure_marker_path(target: &Path) -> Result<PathBuf> {
    let file_name = target
        .file_name()
        .context("running executable has no file name")?;
    let mut marker_name = OsString::from(file_name);
    marker_name.push(".update-failed");
    Ok(target.with_file_name(marker_name))
}

#[cfg(windows)]
fn clear_current_replacement_marker() -> Result<()> {
    let target = std::env::current_exe().context("could not locate the running tg executable")?;
    remove_safe_failure_marker(&replacement_failure_marker_path(&target)?)
}

#[cfg(windows)]
fn remove_safe_failure_marker(marker: &Path) -> Result<()> {
    match fs::symlink_metadata(marker) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(marker).context("failed to clear the old update failure marker")
        }
        Ok(_) => bail!("refusing to remove an unsafe update failure marker"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("failed to inspect the update failure marker"),
    }
}

fn update_marker_path() -> Result<PathBuf> {
    let settings = Settings::path()?;
    Ok(settings.with_file_name("update-check"))
}

fn check_is_due(path: &Path, channel: ReleaseChannel, now: u64) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error).context("failed to inspect update-check marker"),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 128 {
        return Ok(true);
    }
    let contents = fs::read_to_string(path).context("failed to read update-check marker")?;
    let Some((stored_channel, stored_time)) = contents.trim().split_once('\t') else {
        return Ok(true);
    };
    if stored_channel != channel_name(channel) {
        return Ok(true);
    }
    let Ok(stored_time) = stored_time.parse::<u64>() else {
        return Ok(true);
    };
    Ok(stored_time > now || now - stored_time >= CHECK_INTERVAL.as_secs())
}

fn record_check_success(path: &Path, channel: ReleaseChannel, now: u64) -> Result<()> {
    let parent = path.parent().context("update-check marker has no parent")?;
    let created = !parent.exists();
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let parent_metadata = fs::symlink_metadata(parent)
        .with_context(|| format!("failed to inspect {}", parent.display()))?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        bail!("update-check directory is not a real directory");
    }
    #[cfg(unix)]
    if created {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to protect {}", parent.display()))?;
    }
    let temporary = unique_child(parent, ".update-check", ".tmp")?;
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
        writeln!(file, "{}\t{now}", channel_name(channel))
            .context("failed to write update-check marker")?;
        file.sync_all()
            .context("failed to sync update-check marker")?;
        replace_marker(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        drop(fs::remove_file(&temporary));
    }
    result
}

#[cfg(not(windows))]
fn replace_marker(temporary: &Path, path: &Path) -> Result<()> {
    fs::rename(temporary, path).context("failed to replace update-check marker")
}

#[cfg(windows)]
fn replace_marker(temporary: &Path, path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path).context("failed to clear old update-check marker")?;
        }
        Ok(_) => bail!("refusing to replace an unsafe update-check marker"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("failed to inspect update-check marker"),
    }
    fs::rename(temporary, path).context("failed to install update-check marker")
}

const fn channel_name(channel: ReleaseChannel) -> &'static str {
    match channel {
        ReleaseChannel::Stable => "stable",
        ReleaseChannel::Prerelease => "prerelease",
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn create(label: &str) -> Result<Self> {
        let root = std::env::temp_dir();
        let path = unique_child(&root, &format!("termgram-{label}"), "")?;
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.0));
    }
}

struct JsonCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> JsonCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn parse_releases(mut self) -> Result<Vec<Release>> {
        self.whitespace();
        let releases = match self.peek() {
            Some(b'[') => self.parse_release_array()?,
            Some(b'{') => self.parse_release_object()?.into_iter().collect(),
            _ => bail!("GitHub release metadata is not an object or array"),
        };
        self.whitespace();
        if self.position != self.bytes.len() {
            bail!("GitHub release metadata has trailing content");
        }
        Ok(releases)
    }

    fn parse_release_array(&mut self) -> Result<Vec<Release>> {
        self.expect(b'[')?;
        let mut releases = Vec::new();
        self.whitespace();
        if self.consume(b']') {
            return Ok(releases);
        }
        loop {
            if let Some(release) = self.parse_release_object()? {
                releases.push(release);
            }
            self.whitespace();
            if self.consume(b']') {
                return Ok(releases);
            }
            self.expect(b',')?;
        }
    }

    fn parse_release_object(&mut self) -> Result<Option<Release>> {
        self.whitespace();
        self.expect(b'{')?;
        let mut tag = None;
        let mut prerelease = None;
        let mut draft = None;
        self.whitespace();
        if !self.consume(b'}') {
            loop {
                self.whitespace();
                let key = self.parse_ascii_string()?;
                self.whitespace();
                self.expect(b':')?;
                self.whitespace();
                match key.as_str() {
                    "tag_name" => tag = Some(self.parse_ascii_string()?),
                    "prerelease" => prerelease = Some(self.parse_bool()?),
                    "draft" => draft = Some(self.parse_bool()?),
                    _ => self.skip_value(0)?,
                }
                self.whitespace();
                if self.consume(b'}') {
                    break;
                }
                self.expect(b',')?;
            }
        }
        let tag = tag.context("GitHub release omitted tag_name")?;
        let Some(version) = tag.strip_prefix('v') else {
            return Ok(None);
        };
        let Some(height) = parse_version(version) else {
            return Ok(None);
        };
        Ok(Some(Release {
            version: version.to_owned(),
            height,
            prerelease: prerelease.context("GitHub release omitted prerelease")?,
            draft: draft.context("GitHub release omitted draft")?,
        }))
    }

    fn skip_value(&mut self, depth: u8) -> Result<()> {
        if depth > 64 {
            bail!("GitHub release metadata is nested too deeply");
        }
        self.whitespace();
        match self.peek() {
            Some(b'"') => self.skip_string(),
            Some(b'{') => {
                self.position += 1;
                self.whitespace();
                if self.consume(b'}') {
                    return Ok(());
                }
                loop {
                    self.skip_string()?;
                    self.whitespace();
                    self.expect(b':')?;
                    self.skip_value(depth + 1)?;
                    self.whitespace();
                    if self.consume(b'}') {
                        return Ok(());
                    }
                    self.expect(b',')?;
                    self.whitespace();
                }
            }
            Some(b'[') => {
                self.position += 1;
                self.whitespace();
                if self.consume(b']') {
                    return Ok(());
                }
                loop {
                    self.skip_value(depth + 1)?;
                    self.whitespace();
                    if self.consume(b']') {
                        return Ok(());
                    }
                    self.expect(b',')?;
                }
            }
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'n') => self.literal(b"null"),
            Some(b'-' | b'0'..=b'9') => self.skip_number(),
            _ => bail!("invalid JSON value in GitHub release metadata"),
        }
    }

    fn parse_ascii_string(&mut self) -> Result<String> {
        self.expect(b'"')?;
        let mut value = String::new();
        loop {
            let byte = self.next().context("unterminated JSON string")?;
            match byte {
                b'"' => return Ok(value),
                b'\\' => {
                    let escaped = self.next().context("unterminated JSON escape")?;
                    value.push(match escaped {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{0008}',
                        b'f' => '\u{000c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        _ => bail!("unsupported escape in GitHub release metadata"),
                    });
                }
                0x20..=0x7e => value.push(char::from(byte)),
                _ => bail!("non-ASCII release field in GitHub metadata"),
            }
        }
    }

    fn skip_string(&mut self) -> Result<()> {
        self.expect(b'"')?;
        loop {
            match self.next().context("unterminated JSON string")? {
                b'"' => return Ok(()),
                b'\\' => match self.next().context("unterminated JSON escape")? {
                    b'u' => {
                        for _ in 0..4 {
                            if !self.next().is_some_and(|byte| byte.is_ascii_hexdigit()) {
                                bail!("invalid Unicode escape in GitHub metadata");
                            }
                        }
                    }
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                    _ => bail!("invalid JSON escape in GitHub metadata"),
                },
                0x00..=0x1f => bail!("control byte in GitHub release metadata"),
                _ => {}
            }
        }
    }

    fn parse_bool(&mut self) -> Result<bool> {
        if self.remaining().starts_with(b"true") {
            self.position += 4;
            Ok(true)
        } else if self.remaining().starts_with(b"false") {
            self.position += 5;
            Ok(false)
        } else {
            bail!("invalid Boolean in GitHub release metadata")
        }
    }

    fn skip_number(&mut self) -> Result<()> {
        let start = self.position;
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
        {
            self.position += 1;
        }
        if self.position == start {
            bail!("invalid number in GitHub release metadata");
        }
        Ok(())
    }

    fn literal(&mut self, literal: &[u8]) -> Result<()> {
        if !self.remaining().starts_with(literal) {
            bail!("invalid literal in GitHub release metadata");
        }
        self.position += literal.len();
        Ok(())
    }

    fn whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.position += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<()> {
        self.whitespace();
        if self.consume(byte) {
            Ok(())
        } else {
            bail!("invalid GitHub release metadata")
        }
    }

    fn consume(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn remaining(&self) -> &[u8] {
        &self.bytes[self.position..]
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        check_is_due, checksum_for_asset, current_platform, parse_version, select_update,
        AutomaticCheckThrottle, JsonCursor, Release, UpdateStatus, CHECK_INTERVAL,
    };
    use crate::config::ReleaseChannel;

    #[test]
    fn updater_selects_the_native_release_asset() {
        let expected = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => "linux",
            ("linux", "aarch64") => "linux-aarch64",
            ("macos", "aarch64") => "macos",
            ("macos", "x86_64") => "macos-x86_64",
            ("windows", "x86_64") => "windows",
            ("windows", "aarch64") => "windows-aarch64",
            _ => {
                assert!(current_platform().is_err());
                return;
            }
        };
        assert_eq!(
            current_platform().expect("supported platform").asset_label,
            expected
        );
    }

    #[test]
    fn parses_release_objects_without_confusing_nested_asset_fields() {
        let json = br#"[
          {"tag_name":"v0.1.8","draft":false,"prerelease":true,
           "assets":[{"name":"archive","nested":{"draft":true}}]},
          {"tag_name":"v0.1.7","draft":false,"prerelease":false,"assets":[]}
        ]"#;
        let releases = JsonCursor::new(json).parse_releases().expect("releases");
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].height, 8);
        assert!(releases[0].prerelease);
        assert!(!releases[1].prerelease);
    }

    #[test]
    fn ignores_release_tags_outside_the_exact_v0_1_line() {
        let json = br#"[
          {"tag_name":"0.1.10","draft":false,"prerelease":false},
          {"tag_name":"v1.0.0","draft":false,"prerelease":false},
          {"tag_name":"v0.1.9","draft":false,"prerelease":false}
        ]"#;
        let releases = JsonCursor::new(json).parse_releases().expect("releases");
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].version, "0.1.9");
    }

    #[test]
    fn channels_choose_highest_allowed_height_without_downgrading() {
        let releases = vec![
            Release {
                version: "0.1.9".to_owned(),
                height: 9,
                prerelease: true,
                draft: false,
            },
            Release {
                version: "0.1.8".to_owned(),
                height: 8,
                prerelease: false,
                draft: false,
            },
        ];
        assert_eq!(
            select_update(&releases, ReleaseChannel::Stable, "0.1.7").expect("stable"),
            UpdateStatus::Available {
                version: "0.1.8".to_owned(),
                url: "https://github.com/iebb/termgram/releases/tag/v0.1.8".to_owned(),
            }
        );
        assert_eq!(
            select_update(&releases, ReleaseChannel::Prerelease, "0.1.8").expect("prerelease"),
            UpdateStatus::Available {
                version: "0.1.9".to_owned(),
                url: "https://github.com/iebb/termgram/releases/tag/v0.1.9".to_owned(),
            }
        );
        assert_eq!(
            select_update(&releases, ReleaseChannel::Stable, "0.1.10").expect("no downgrade"),
            UpdateStatus::UpToDate
        );
    }

    #[test]
    fn semantic_height_parser_is_strict() {
        assert_eq!(parse_version("0.1.42"), Some(42));
        assert_eq!(parse_version("0.1.0"), Some(0));
        for invalid in ["v0.1.2", "0.2.2", "0.1.02", "0.1.-1", "0.1."] {
            assert_eq!(parse_version(invalid), None);
        }
    }

    #[test]
    fn checksum_manifest_requires_one_exact_asset() {
        let manifest = concat!(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  other\n",
            "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB *wanted\n",
        );
        assert_eq!(
            checksum_for_asset(manifest, "wanted").expect("checksum"),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        assert!(checksum_for_asset("bad  wanted\n", "wanted").is_err());
        assert!(checksum_for_asset(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  wanted\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  wanted\n",
            "wanted"
        )
        .is_err());
    }

    #[test]
    fn update_attempt_marker_is_channel_specific_and_daily() {
        let path = std::env::temp_dir().join(format!(
            "termgram-update-marker-test-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"stable\t1000\n").expect("marker");
        assert!(!check_is_due(Path::new(&path), ReleaseChannel::Stable, 1001).expect("fresh"));
        assert!(
            check_is_due(Path::new(&path), ReleaseChannel::Prerelease, 1001)
                .expect("channel changed")
        );
        assert!(check_is_due(
            Path::new(&path),
            ReleaseChannel::Stable,
            1000 + CHECK_INTERVAL.as_secs()
        )
        .expect("expired"));
        assert!(check_is_due(Path::new(&path), ReleaseChannel::Stable, 999)
            .expect("clock moved backwards"));
        std::fs::remove_file(path).expect("remove marker");
    }

    #[test]
    fn failed_automatic_checks_are_throttled_only_in_the_current_process() {
        let mut throttle = AutomaticCheckThrottle::new();
        assert!(throttle.begin(ReleaseChannel::Stable));
        assert!(!throttle.begin(ReleaseChannel::Stable));
        assert!(throttle.begin(ReleaseChannel::Prerelease));

        throttle.finish(ReleaseChannel::Stable, false);
        assert!(!throttle.begin(ReleaseChannel::Stable));

        throttle.finish(ReleaseChannel::Prerelease, true);
        assert!(throttle.begin(ReleaseChannel::Prerelease));
    }
}
