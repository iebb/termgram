//! When the account reports itself online. The rules follow Telegram
//! Desktop's `Api::Updates::updateOnline` (`api/api_updates.cpp`) and the
//! defaults in `mtproto/mtproto_config.h`; no source is copied. A terminal
//! only sees its own input, so "idle" means no key, mouse or paste here.

use serde::Deserialize;
use std::time::{Duration, Instant};

/// Desktop's `onlineFocusTimeout`: focus must last this long to count.
const FOCUS_DELAY: Duration = Duration::from_secs(1);
/// Desktop's `offlineBlurTimeout`: grace period after losing focus.
const BLUR_TIMEOUT: Duration = Duration::from_secs(5);
/// Desktop's `offlineIdleTimeout`: no input for this long means away.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Configuration {
    /// False never reports a status, like a client that is never opened.
    pub online: bool,
}

impl Default for Configuration {
    fn default() -> Self {
        Self { online: true }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct State {
    focused: bool,
    focus_changed: Instant,
    last_input: Instant,
    /// The worker accepts commands once the account is authorized.
    ready: bool,
    reported: bool,
}

impl State {
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            focused: true,
            focus_changed: now,
            last_input: now,
            ready: false,
            reported: false,
        }
    }

    pub fn input(&mut self, now: Instant) {
        self.last_input = now;
    }

    pub fn focus(&mut self, focused: bool, now: Instant) {
        if self.focused != focused {
            self.focused = focused;
            self.focus_changed = now;
        }
        if focused {
            self.last_input = now;
        }
    }

    /// A new worker starts offline, whatever the previous one reported.
    pub fn worker_ready(&mut self, ready: bool) {
        self.ready = ready;
        self.reported = false;
    }

    /// The status wanted at `now`, and when that answer next changes.
    fn wanted(&self, enabled: bool, now: Instant) -> (bool, Option<Instant>) {
        if !enabled {
            return (false, None);
        }
        let idle = self.last_input + IDLE_TIMEOUT;
        let (from, until) = if self.focused {
            (self.focus_changed + FOCUS_DELAY, idle)
        } else if self.reported {
            (
                self.focus_changed,
                idle.min(self.focus_changed + BLUR_TIMEOUT),
            )
        } else {
            return (false, None);
        };
        if now >= until {
            (false, None)
        } else if now < from {
            (false, Some(from))
        } else {
            (true, Some(until))
        }
    }

    /// The status to report now, when it differs from the last one reported.
    pub fn change(&mut self, enabled: bool, now: Instant) -> Option<bool> {
        let online = self.ready && self.wanted(enabled, now).0;
        (online != self.reported).then(|| {
            self.reported = online;
            online
        })
    }

    #[must_use]
    pub fn deadline(&self, enabled: bool, now: Instant) -> Option<Instant> {
        self.wanted(enabled, now).1.filter(|_| self.ready)
    }
}

#[cfg(test)]
mod tests {
    use super::State;
    use std::time::{Duration, Instant};

    #[test]
    fn online_follows_focus_input_and_configuration() {
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        let mut state = State::new(start);
        assert_eq!(state.change(true, at(2_000)), None, "worker not ready");
        state.worker_ready(true);
        assert_eq!(state.change(true, at(500)), None, "focus must settle");
        assert_eq!(state.deadline(true, at(500)), Some(at(1_000)));
        assert_eq!(state.change(true, at(1_000)), Some(true));
        assert_eq!(state.change(true, at(29_000)), None);
        assert_eq!(state.change(true, at(30_000)), Some(false), "idle");
        state.input(at(40_000));
        assert_eq!(state.change(true, at(40_000)), Some(true));

        state.focus(false, at(41_000));
        assert_eq!(state.change(true, at(45_000)), None, "blur grace");
        assert_eq!(state.deadline(true, at(45_000)), Some(at(46_000)));
        assert_eq!(state.change(true, at(46_000)), Some(false));
        state.focus(true, at(50_000));
        assert_eq!(state.change(true, at(51_000)), Some(true));
        assert_eq!(state.change(false, at(51_000)), Some(false), "disabled");

        let mut hidden = State::new(start);
        hidden.worker_ready(true);
        assert_eq!(hidden.change(false, at(5_000)), None, "never reported");
    }
}
