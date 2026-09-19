//! Yazi owns terminal input, capability detection, and platform restoration.
//! Ratatui uses the same TTY writer exclusively for output; it never reads input.

use std::io::{self, Write};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::{
    task::JoinHandle,
    time::{Instant, sleep_until},
};
use yazi_emulator::{Deinit, EMULATOR, Mux};
use yazi_term::{
    TERM,
    event::{Event, Report},
    stream::EventStream,
};
use yazi_tty::{
    TTY, TtyWriter,
    sequence::{
        DisableBracketedPaste, DisableColorSchemeUpdates, DisableFocusChange, DisableMouseCapture,
        EnableBracketedPaste, EnableColorSchemeUpdates, EnableFocusChange, EnableMouseCapture,
        EndSyncUpdate, EnterAlternateScreen, LeaveAlternateScreen, PopKeyboardFlags,
        PushKeyboardFlags, RequestCellPixelSize, RestoreCursorStyle, ShowCursor,
    },
};

pub type AppTerminal = Terminal<CrosstermBackend<TtyWriter<'static>>>;

static INITIALIZED: OnceLock<Result<(), String>> = OnceLock::new();
static MODES_ACTIVE: AtomicBool = AtomicBool::new(false);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The reader is dropped before the restoration guard. This joins Yazi's
/// platform reader before upstream cleanup can drain any pending replies.
pub struct TerminalGuard {
    terminal: AppTerminal,
    input: EventStream,
    passthrough: Option<JoinHandle<u64>>,
    probe_deadline: Instant,
    allow_passthrough: bool,
    _modes: TerminalModes,
}

struct TerminalModes {
    _deinit: Deinit,
}

impl Drop for TerminalModes {
    fn drop(&mut self) {
        restore_modes();
        // Deinit then drains pending reports and restores original platform modes.
    }
}

/// Restore modes before printing a panic, including in panic=abort builds.
pub fn install_panic_restore_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if matches!(INITIALIZED.get(), Some(Ok(()))) {
            let _ = TERM.source.wake();
            restore_modes();
            EMULATOR.stop();
        }
        previous(info);
    }));
}

impl TerminalGuard {
    /// Enter the full-screen UI with one Yazi input reader.
    ///
    /// # Errors
    /// Returns an error if terminal initialization or mode setup fails.
    pub fn enter() -> Result<Self> {
        INITIALIZED
            .get_or_init(|| {
                // Upstream RoCell globals must be initialized once, in dependency order,
                // before starting the reader. They remain alive for the process lifetime.
                yazi_tty::init();
                yazi_emulator::init();
                yazi_term::setup().map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|error| anyhow!("{error}"))?;

        let modes = TerminalModes {
            _deinit: yazi_emulator::setup()?,
        };
        MODES_ACTIVE.store(true, Ordering::Release);
        let keyboard = PushKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES
            | PushKeyboardFlags::REPORT_ALTERNATE_KEYS
            | PushKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            | PushKeyboardFlags::REPORT_ASSOCIATED_TEXT;
        write!(
            TTY.writer(),
            "{EnterAlternateScreen}{EnableBracketedPaste}{EnableFocusChange}{EnableColorSchemeUpdates}{EnableMouseCapture}{keyboard}"
        )?;
        TTY.writer().flush()?;

        let terminal = Terminal::new(CrosstermBackend::new(TTY.writer()))?;
        let input = EventStream::from(&*TERM);
        Ok(Self {
            terminal,
            input,
            passthrough: None,
            probe_deadline: Instant::now() + PROBE_TIMEOUT,
            // Upstream tmux_setup changes pane and server options. Make that
            // policy explicit rather than hiding it inside capability detection.
            allow_passthrough: std::env::var_os("TERMGRAM_TMUX_PASSTHROUGH")
                .is_some_and(|value| value == "1"),
            _modes: modes,
        })
    }

    pub fn terminal_mut(&mut self) -> &mut AppTerminal {
        &mut self.terminal
    }

    /// Consume protocol reports here so they can never become draft text.
    /// The receiver, deadline, and reprobe task survive select! cancellation.
    pub async fn next_event(&mut self) -> Option<io::Result<Event>> {
        loop {
            tokio::select! {
                event = self.input.next() => {
                    match event {
                        Some(Ok(Event::Report(report))) => self.report(&report),
                        Some(Ok(event @ Event::Resize(_))) => {
                            // Font changes can change pixel dimensions without changing
                            // the terminal brand or negotiated graphics protocol.
                            let _ = write!(TTY.writer(), "{RequestCellPixelSize}");
                            let _ = TTY.writer().flush();
                            return Some(Ok(event));
                        }
                        event => return event,
                    }
                }
                result = async {
                    match self.passthrough.as_mut() {
                        Some(task) => task.await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.passthrough = None;
                    if let Ok(id) = result
                        && EMULATOR.probe.pending().is_some_and(|current| current.get() == id)
                        && EMULATOR.needs_passthrough()
                    {
                        if let Err(error) = EMULATOR.restart() {
                            EMULATOR.probe.complete();
                            return Some(Err(io::Error::other(error.to_string())));
                        }
                        self.probe_deadline = Instant::now() + PROBE_TIMEOUT;
                    }
                }
                () = sleep_until(self.probe_deadline), if EMULATOR.probe.pending().is_some() => {
                    EMULATOR.probe.complete();
                    if let Some(task) = self.passthrough.take() {
                        task.abort();
                    }
                }
            }
        }
    }

    // Adapted from yazi-actor app/report.rs and app/passthrough.rs at the
    // revision recorded in vendor/README.md. Keep the two-stage probe contract.
    fn report(&mut self, report: &Report) {
        EMULATOR.apply(report);
        if !report.is_da_1() || EMULATOR.probe.pending().is_none() {
            return;
        }
        if self.allow_passthrough && EMULATOR.needs_passthrough() {
            if self.passthrough.is_none() {
                let id = EMULATOR.probe.id.get().get();
                self.passthrough = Some(tokio::spawn(async move {
                    Mux::tmux_setup().await;
                    id
                }));
            }
        } else {
            EMULATOR.probe.complete();
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Some(task) = self.passthrough.take() {
            task.abort();
        }
    }
}

// Adapted from yazi-tui Raterm::stop. Only reset modes that Termgram enables;
// platform mode restoration remains owned by yazi-emulator::Deinit.
fn restore_modes() {
    if !MODES_ACTIVE.swap(false, Ordering::AcqRel) {
        return;
    }
    crate::media::cleanup();
    let cursor = RestoreCursorStyle {
        blink: EMULATOR.cursor_blink.get(),
        shape: EMULATOR.cursor_shape.get(),
    };
    let _ = write!(
        TTY.writer(),
        "{EndSyncUpdate}{DisableMouseCapture}{PopKeyboardFlags}{DisableColorSchemeUpdates}{DisableFocusChange}{DisableBracketedPaste}{cursor}{ShowCursor}{LeaveAlternateScreen}"
    );
    let _ = TTY.writer().flush();
}
