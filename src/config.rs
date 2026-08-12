use std::env;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use directories::ProjectDirs;

const EMBEDDED_API_ID: Option<&str> = option_env!("TERMGRAM_EMBEDDED_API_ID");
const EMBEDDED_API_HASH: Option<&str> = option_env!("TERMGRAM_EMBEDDED_API_HASH");

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

    use super::{choose_default_session_path, credential, Config};

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
