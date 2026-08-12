use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use termgram::app::{AppState, Screen};
use termgram::config::Config;
use termgram::event::{AppEvent, NetworkEvent, TelegramCommand};
use termgram::telegram::{self, TelegramHandle};
use termgram::terminal::{install_panic_restore_hook, TerminalGuard};
use termgram::ui;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};

enum RuntimeEvent {
    Terminal(Option<io::Result<Event>>),
    Network(Box<Option<NetworkEvent>>),
    ShutdownSignal(io::Result<()>),
    Tick,
}

const ANIMATION_INTERVAL: Duration = Duration::from_millis(120);
const MAX_PENDING_COMMANDS: usize = 64;

#[tokio::main(flavor = "current_thread")]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    if matches!(std::env::args().nth(1).as_deref(), Some("--version" | "-V")) {
        println!("tg {}", termgram::VERSION);
        return Ok(());
    }

    install_panic_restore_hook();
    let mut terminal = TerminalGuard::enter().context("failed to initialize the terminal")?;
    let mut app = AppState::new();

    let (mut commands, mut events, mut worker) = match Config::load() {
        Ok(config) => {
            let TelegramHandle {
                commands,
                events,
                task,
            } = telegram::spawn(config);
            (Some(commands), Some(events), Some(task))
        }
        Err(error) => {
            app.handle_network(NetworkEvent::Fatal(format!("{error:#}")));
            (None, None, None)
        }
    };

    let mut input = EventStream::new();
    let mut shutdown_signal = Box::pin(wait_for_shutdown_signal());
    let mut pending_commands = VecDeque::new();
    let mut redraw = true;
    let mut runtime_error: Option<anyhow::Error> = None;

    loop {
        if redraw {
            if app.take_force_redraw() {
                if let Err(error) = terminal
                    .terminal_mut()
                    .clear()
                    .context("failed to redraw the terminal")
                {
                    runtime_error = Some(error);
                    break;
                }
            }
            if let Err(error) = terminal
                .terminal_mut()
                .draw(|frame| ui::render(frame, &mut app))
                .context("failed to draw the interface")
            {
                runtime_error = Some(error);
                break;
            }
        }

        if app.should_quit {
            break;
        }

        let animation_tick = wait_for_animation_tick(app.needs_animation());
        let runtime_event = if let Some(network) = events.as_mut() {
            tokio::select! {
                event = input.next() => RuntimeEvent::Terminal(event),
                event = network.recv() => RuntimeEvent::Network(Box::new(event)),
                result = &mut shutdown_signal => RuntimeEvent::ShutdownSignal(result),
                () = animation_tick => RuntimeEvent::Tick,
            }
        } else {
            tokio::select! {
                event = input.next() => RuntimeEvent::Terminal(event),
                result = &mut shutdown_signal => RuntimeEvent::ShutdownSignal(result),
                () = animation_tick => RuntimeEvent::Tick,
            }
        };
        redraw = !matches!(&runtime_event, RuntimeEvent::Tick) || app.needs_animation();

        let outgoing = match runtime_event {
            RuntimeEvent::Terminal(Some(Ok(Event::Key(key)))) => app.update(AppEvent::Key(key)),
            RuntimeEvent::Terminal(Some(Ok(Event::Mouse(mouse)))) => {
                app.update(AppEvent::Mouse(mouse))
            }
            RuntimeEvent::Terminal(Some(Ok(Event::Paste(text)))) => {
                app.update(AppEvent::Paste(text))
            }
            RuntimeEvent::Terminal(Some(Ok(Event::FocusGained))) => {
                app.update(AppEvent::TerminalFocus(true))
            }
            RuntimeEvent::Terminal(Some(Ok(Event::FocusLost))) => {
                app.update(AppEvent::TerminalFocus(false))
            }
            RuntimeEvent::Terminal(Some(Ok(_))) => Vec::new(),
            RuntimeEvent::Terminal(Some(Err(error))) => {
                runtime_error = Some(anyhow::Error::from(error).context("terminal input failed"));
                app.handle_action(termgram::input::KeyAction::Quit)
            }
            RuntimeEvent::Terminal(None) => app.handle_action(termgram::input::KeyAction::Quit),
            RuntimeEvent::Network(event) => {
                if let Some(event) = *event {
                    app.update(AppEvent::Network(event))
                } else {
                    events = None;
                    commands = None;
                    pending_commands.clear();
                    if matches!(&app.screen, Screen::Fatal(_)) {
                        Vec::new()
                    } else {
                        app.handle_network(NetworkEvent::Fatal(
                            "Telegram connection closed unexpectedly".to_owned(),
                        ))
                    }
                }
            }
            RuntimeEvent::ShutdownSignal(Ok(())) => {
                app.handle_action(termgram::input::KeyAction::Quit)
            }
            RuntimeEvent::ShutdownSignal(Err(error)) => {
                runtime_error = Some(
                    anyhow::Error::from(error).context("failed to listen for a shutdown signal"),
                );
                app.handle_action(termgram::input::KeyAction::Quit)
            }
            RuntimeEvent::Tick => app.update(AppEvent::Tick),
        };

        dispatch(&mut app, &mut commands, &mut pending_commands, outgoing);
    }

    drop(terminal);
    drop(commands.take());
    // Stop back-pressuring a worker that may be trying to report its final
    // status while the UI is no longer consuming network events.
    drop(events.take());
    if let Some(mut task) = worker.take() {
        if timeout(Duration::from_secs(2), &mut task).await.is_err() {
            task.abort();
            drop(task.await);
        }
    }

    runtime_error.map_or(Ok(()), Err)
}

async fn wait_for_animation_tick(enabled: bool) {
    if enabled {
        sleep(ANIMATION_INTERVAL).await;
    } else {
        std::future::pending::<()>().await;
    }
}

fn dispatch(
    app: &mut AppState,
    commands: &mut Option<mpsc::Sender<TelegramCommand>>,
    pending: &mut VecDeque<TelegramCommand>,
    outgoing: Vec<TelegramCommand>,
) {
    for command in outgoing {
        if pending.len() >= MAX_PENDING_COMMANDS {
            reject_overflow(app, command);
        } else {
            pending.push_back(command);
        }
    }
    let Some(sender) = commands.clone() else {
        pending.clear();
        return;
    };
    while let Some(command) = pending.pop_front() {
        match sender.try_send(command) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(command)) => {
                pending.push_front(command);
                break;
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                *commands = None;
                pending.clear();
                app.handle_network(NetworkEvent::Fatal(
                    "Telegram worker stopped unexpectedly".to_owned(),
                ));
                break;
            }
        }
    }
}

fn reject_overflow(app: &mut AppState, command: TelegramCommand) {
    let event = match command {
        TelegramCommand::SendMessage {
            chat_id,
            local_id,
            text,
        } => NetworkEvent::SendFailed {
            chat_id,
            local_id,
            text,
            error: "Telegram command queue is busy".to_owned(),
        },
        TelegramCommand::SendAttachment {
            chat_id,
            local_id,
            path,
            caption,
            as_photo,
        } => NetworkEvent::AttachmentSendFailed {
            chat_id,
            local_id,
            path,
            caption,
            as_photo,
            error: "Telegram command queue is busy".to_owned(),
        },
        TelegramCommand::DownloadAttachment {
            chat_id,
            message_id,
        } => NetworkEvent::AttachmentDownloadFailed {
            chat_id,
            message_id,
            error: "Telegram command queue is busy".to_owned(),
        },
        TelegramCommand::ResolveTelegramLink { url } => NetworkEvent::LinkFailed {
            url,
            error: "Telegram command queue is busy".to_owned(),
        },
        TelegramCommand::LoadHistory {
            chat_id,
            request_id,
        } => NetworkEvent::HistoryFailed {
            chat_id,
            request_id,
            error: "Telegram command queue is busy".to_owned(),
        },
        TelegramCommand::MarkRead { chat_id } => NetworkEvent::ReadMarkFailed {
            chat_id,
            error: "Telegram command queue is busy".to_owned(),
        },
        TelegramCommand::RefreshDialogs => {
            NetworkEvent::DialogsFailed("Telegram command queue is busy".to_owned())
        }
        TelegramCommand::SubmitPhone(_)
        | TelegramCommand::SubmitCode(_)
        | TelegramCommand::SubmitPassword(_)
        | TelegramCommand::RestartAuth => {
            NetworkEvent::Fatal("Telegram command queue is busy; restart sign-in".to_owned())
        }
        TelegramCommand::Shutdown => return,
    };
    app.handle_network(event);
}

async fn wait_for_shutdown_signal() -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}
