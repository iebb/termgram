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
