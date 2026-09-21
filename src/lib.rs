pub mod actions;
pub mod app;
pub mod appearance;
pub mod cache;
pub mod chat_discovery;
pub mod chat_info;
pub mod cloud_search;
pub mod commands;
pub mod completion;
pub mod config;
pub mod deletion;
pub mod drafts;
pub mod editing;
pub mod entities;
pub mod event;
pub mod folders;
pub mod forwarding;
pub mod input;
pub mod invites;
pub mod keymap;
pub mod media;
pub mod model;
pub mod notifications;
pub mod pins;
pub mod polls;
pub mod reactions;
pub mod search;
pub mod sidebar;
pub mod staging;
pub mod statusline;
pub mod telegram;
pub mod terminal;
pub mod transcript;
pub mod ui;
pub mod update;

/// CI can override the version. Git source installs use the same commit-height
/// versioning; archives without Git metadata use the package development version.
pub const VERSION: &str = match option_env!("TERMGRAM_BUILD_VERSION") {
    Some(version) => version,
    None => env!("TERMGRAM_SOURCE_VERSION"),
};

/// Human-readable build identity, separate from the updater's semantic version.
#[must_use]
pub fn version_description(styled: bool) -> String {
    let metadata = |value| match value {
        "VERGEN_IDEMPOTENT_OUTPUT" => "unknown",
        value => value,
    };
    let sha = metadata(env!("VERGEN_GIT_SHA"));
    let branch = metadata(env!("VERGEN_GIT_BRANCH"));
    let dirty = if env!("VERGEN_GIT_DIRTY") == "true" {
        "*"
    } else {
        ""
    };
    let commit = format!("{sha}{dirty}");
    let os = match std::env::consts::OS {
        "macos" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        os => os,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        arch => arch,
    };
    let label_style = if styled {
        anstyle::AnsiColor::Cyan.on_default().bold()
    } else {
        anstyle::Style::new()
    };
    [
        ("version", VERSION),
        ("commit", commit.as_str()),
        ("branch", branch),
        ("os", os),
        ("arch", arch),
        ("build", env!("TERMGRAM_BUILD_NUMBER")),
    ]
    .into_iter()
    .map(|(label, value)| format!("{label_style}{label:<7}{label_style:#}  {value}"))
    .collect::<Vec<_>>()
    .join("\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_has_three_numeric_components() {
        let components = super::VERSION.split('.').collect::<Vec<_>>();
        assert_eq!(components.len(), 3);
        assert!(
            components
                .iter()
                .all(|component| component.parse::<u64>().is_ok())
        );
    }
}

mod clipboard;

pub mod read_state;
