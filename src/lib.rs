pub mod app;
pub mod config;
pub mod event;
pub mod input;
pub mod model;
pub mod telegram;
pub mod terminal;
pub mod ui;
pub mod update;

/// Version embedded into distributable binaries by CI. Source builds fall
/// back to the package's base development version.
pub const VERSION: &str = match option_env!("TERMGRAM_BUILD_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
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
        assert!(components
            .iter()
            .all(|component| component.parse::<u64>().is_ok()));
    }
}
