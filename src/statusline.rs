//! Declarative status items and ephemeral observations; no Lua runs while drawing.

use serde::Deserialize;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Item {
    Mode,
    App,
    Account,
    Connection,
    Latency,
    Dc,
    Position,
    Message,
    Notifications,
    Unread,
    Activity,
    Context,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Configuration {
    pub enabled: bool,
    pub left: Vec<Item>,
    pub right: Vec<Item>,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            enabled: true,
            left: vec![
                Item::Mode,
                Item::App,
                Item::Account,
                Item::Activity,
                Item::Message,
                Item::Context,
            ],
            right: vec![
                Item::Notifications,
                Item::Unread,
                Item::Connection,
                Item::Latency,
                Item::Dc,
                Item::Position,
            ],
        }
    }
}

impl Configuration {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        let mut seen = Vec::new();
        for item in self.left.iter().chain(&self.right) {
            anyhow::ensure!(
                !seen.contains(item),
                "statusline item {item:?} appears more than once"
            );
            seen.push(*item);
        }
        Ok(())
    }
    #[must_use]
    pub fn measures_latency(&self) -> bool {
        self.enabled
            && self
                .left
                .iter()
                .chain(&self.right)
                .any(|item| *item == Item::Latency)
    }
}

/// Includes SDK scheduling/retries and network time, not message delivery latency.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Latency {
    pub started: Instant,
    pub elapsed: Duration,
}

impl Latency {
    #[must_use]
    pub fn expired(self) -> bool {
        self.started.elapsed() >= Duration::from_secs(90)
    }
}

#[derive(Clone, Debug)]
pub struct Metrics {
    pub dc_id: Option<i32>,
    pub latency: Option<Latency>,
    /// Reject measurements begun before a disconnect, including in-flight probes.
    pub reset_at: Instant,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            dc_id: None,
            latency: None,
            reset_at: Instant::now(),
        }
    }
}
