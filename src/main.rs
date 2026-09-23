use std::collections::VecDeque;
use std::io;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use termgram::app::{AppState, Screen};
use termgram::config::{Config, ReleaseChannel, Settings};
use termgram::event::{AppEvent, ConnectionStatus, NetworkEvent, TelegramCommand};
use termgram::media::PreviewRenderer;
use termgram::telegram::{self, TelegramHandle};
use termgram::terminal::{TerminalGuard, install_panic_restore_hook};
use termgram::ui;
use termgram::update::{self, UpdateOutcome, UpdateStatus};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, timeout};
use yazi_term::event::Event;

enum RuntimeEvent {
    Configuration(Result<termgram::keymap::Keymap, String>),
    Terminal(Option<io::Result<Event>>),
    Network(Box<Option<NetworkEvent>>),
    UpdateCheck {
        channel: ReleaseChannel,
        result: UpdateCheckResult,
    },
    ShutdownSignal(io::Result<()>),
    Preview(Result<Vec<termgram::media::MediaFailure>>),
    DraftError(String),
    Clipboard(termgram::terminal::clipboard::Activity),
    Notification(termgram::notifications::delivery::Activity),
    Tick,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandLine {
    Tui,
    Version,
    Help,
    Update(Option<ReleaseChannel>),
}

type UpdateCheckResult = std::result::Result<Option<UpdateStatus>, String>;

struct UpdateCheck {
    channel: ReleaseChannel,
    receiver: oneshot::Receiver<UpdateCheckResult>,
}

const ANIMATION_INTERVAL: Duration = Duration::from_millis(120);
const MAX_PENDING_COMMANDS: usize = 64;

#[tokio::main(flavor = "current_thread")]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    let command_line = parse_arguments(std::env::args().skip(1))?;
    let replacement_warning = update::pending_replacement_warning().map_or_else(
        |error| {
            Some(format!(
                "Could not inspect the previous update result: {error:#}"
            ))
        },
        |warning| warning.map(str::to_owned),
    );
    match command_line {
        CommandLine::Version => {
            anstream::println!("{}", termgram::version_description(true));
            return Ok(());
        }
        CommandLine::Help => {
            print_help();
            return Ok(());
        }
        CommandLine::Update(channel) => {
            if let Some(warning) = &replacement_warning {
                eprintln!("Warning: {warning}");
            }
            run_update_command(channel)?;
            return Ok(());
        }
        CommandLine::Tui => {}
    }

    install_panic_restore_hook();
    let mut terminal = TerminalGuard::enter().context("failed to initialize the terminal")?;
    let mut preview = PreviewRenderer::new();
    let (mut app, settings) = load_app_settings();
    let mut startup_warning = replacement_warning;
    let lua_path = std::env::var_os("TERMGRAM_CONFIG")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            Settings::path()
                .ok()
                .map(|path| path.with_file_name("config.lua"))
        });
    app.configuration.path.clone_from(&lua_path);
    if let Some(path) = lua_path {
        match termgram::keymap::Keymap::load(&path) {
            Ok(keymap) => app.keymap = keymap,
            Err(error) => {
                app.configuration.error = Some(termgram::model::sanitize_terminal_line(&format!(
                    "{error:#}"
                )));
                startup_warning = Some(format!("{error:#}; using default bindings"));
            }
        }
    }
    if let Err(error) = app.load_appearance() {
        startup_warning = Some(format!("Could not load appearance preferences: {error:#}"));
    }
    if let Err(error) = app.load_navigation() {
        startup_warning = Some(format!("Could not load navigation preferences: {error:#}"));
    }
    if let Some(warning) = &startup_warning {
        app.handle_network(NetworkEvent::Error(warning.clone()));
    }
    let mut update_check = spawn_update_check(settings);
    let mut update_preferences = settings;

    let mut measurement = None;
    let (mut commands, mut events, mut worker, mut base_config, mut active_chat) =
        match Config::load().map(|mut config| {
            config.measure_latency = app.keymap.statusline.measures_latency();
            config
        }) {
            Ok(config) => match config.for_account(settings.active_account) {
                Ok(selected) => {
                    let TelegramHandle {
                        measure_latency,
                        commands,
                        events,
                        task,
                        active_chat,
                    } = telegram::spawn(selected);
                    measurement = Some(measure_latency);
                    (
                        Some(commands),
                        Some(events),
                        Some(task),
                        Some(config),
                        Some(active_chat),
                    )
                }
                Err(error) => {
                    app.handle_network(NetworkEvent::Fatal(format!("{error:#}")));
                    (None, None, None, Some(config), None)
                }
            },
            Err(error) => {
                app.handle_network(NetworkEvent::Fatal(format!("{error:#}")));
                (None, None, None, None, None)
            }
        };

    let mut draft_writer = base_config.clone().map(termgram::drafts::Writer::spawn);
    let mut shutdown_signal = Box::pin(wait_for_shutdown_signal());
    let mut pending_commands = VecDeque::new();
    let mut clipboard = termgram::terminal::clipboard::Broker::default();
    let mut notifications = termgram::notifications::delivery::Dispatcher::default();
    let mut redraw = true;
    let mut runtime_error: Option<anyhow::Error> = None;
    let mut configuration_load = None;

    loop {
        if let Some(path) = app.take_config_reload() {
            configuration_load = Some(tokio::task::spawn_blocking(move || {
                termgram::keymap::Keymap::reload(&path).map_err(|e| format!("{e:#}"))
            }));
        }
        if let Some(measurement) = &measurement {
            let enabled = app.keymap.statusline.measures_latency();
            measurement.send_if_modified(|current| {
                let changed = *current != enabled;
                *current = enabled;
                changed
            });
        }
        if let Some(config) = &mut base_config {
            config.measure_latency = app.keymap.statusline.measures_latency();
        }
        if let Err(error) =
            terminal.set_clipboard_enabled(app.keymap.attachments.terminal_clipboard)
        {
            runtime_error =
                Some(anyhow::Error::from(error).context("terminal clipboard mode failed"));
            break;
        }
        app.configuration.terminal_clipboard = terminal.clipboard_supported();
        clipboard.configure(terminal.clipboard_supported(), &mut app);
        if let Some(writer) = &draft_writer
            && let Some(snapshot) = app.take_draft_snapshot()
        {
            writer.queue(snapshot);
        }
        if redraw && let Err(error) = draw_interface(&mut terminal, &mut preview, &mut app) {
            runtime_error = Some(error);
            break;
        }

        if app.should_quit {
            break;
        }

        let missing_previews = app
            .media_slots
            .iter()
            .filter_map(|slot| match slot.source {
                termgram::media::MediaSource::Message {
                    chat_id,
                    message_id,
                } => Some((chat_id, message_id)),
                termgram::media::MediaSource::File(_) => None,
            })
            .filter(|key| !app.media_previews.contains_key(key))
            .collect::<Vec<_>>();
        let mut outgoing = app.request_visible_media();
        outgoing.extend(app.request_sticker_thumbs());
        outgoing.extend(app.request_visible_replies());
        outgoing.extend(app.request_visible_read());
        outgoing.extend(app.request_visible_polls());
        outgoing.extend(app.request_visible_reactions());
        outgoing.extend(app.request_visible_chat_info());
        outgoing.extend(app.request_alert_settings());
        notifications.flush(&mut app);
        let outgoing = clipboard.route(&mut app, outgoing);
        clipboard.flush(&mut app);
        dispatch(&mut app, &mut commands, &mut pending_commands, outgoing);
        if missing_previews
            .iter()
            .any(|key| app.media_previews.contains_key(key))
        {
            redraw = true;
            continue;
        }

        let animation_tick = wait_for_animation_tick(app.needs_animation());
        let runtime_event = if let Some(network) = events.as_mut() {
            tokio::select! {
                event = terminal.next_event() => RuntimeEvent::Terminal(event),
                result = wait_for_configuration(&mut configuration_load) => RuntimeEvent::Configuration(result),
                result = preview.finished() => RuntimeEvent::Preview(result),
                error = wait_for_draft_error(&mut draft_writer) => RuntimeEvent::DraftError(error),
                activity = clipboard.next_activity() => RuntimeEvent::Clipboard(activity),
                activity = notifications.next_activity(app.next_alert_deadline().into_iter().chain(app.next_poll_deadline()).chain(app.next_reaction_deadline()).chain(app.next_chat_info_deadline()).min()) => RuntimeEvent::Notification(activity),
                event = network.recv() => RuntimeEvent::Network(Box::new(event)),
                (channel, result) = wait_for_update_check(&mut update_check) => RuntimeEvent::UpdateCheck { channel, result },
                result = &mut shutdown_signal => RuntimeEvent::ShutdownSignal(result),
                () = animation_tick => RuntimeEvent::Tick,
            }
        } else {
            tokio::select! {
                event = terminal.next_event() => RuntimeEvent::Terminal(event),
                result = wait_for_configuration(&mut configuration_load) => RuntimeEvent::Configuration(result),
                result = preview.finished() => RuntimeEvent::Preview(result),
                error = wait_for_draft_error(&mut draft_writer) => RuntimeEvent::DraftError(error),
                activity = clipboard.next_activity() => RuntimeEvent::Clipboard(activity),
                activity = notifications.next_activity(app.next_alert_deadline().into_iter().chain(app.next_poll_deadline()).chain(app.next_reaction_deadline()).chain(app.next_chat_info_deadline()).min()) => RuntimeEvent::Notification(activity),
                (channel, result) = wait_for_update_check(&mut update_check) => RuntimeEvent::UpdateCheck { channel, result },
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
            RuntimeEvent::Terminal(Some(Ok(Event::Clipboard(event)))) => {
                clipboard.event(&mut app, event)
            }
            RuntimeEvent::Terminal(Some(Ok(Event::FocusIn))) => {
                app.update(AppEvent::TerminalFocus(true))
            }
            RuntimeEvent::Terminal(Some(Ok(Event::FocusOut))) => {
                app.update(AppEvent::TerminalFocus(false))
            }
            RuntimeEvent::Terminal(Some(Ok(Event::Resize(_)))) => {
                preview.invalidate();
                app.handle_action(termgram::input::KeyAction::Redraw)
            }
            RuntimeEvent::Terminal(Some(Ok(Event::Report(
                yazi_term::event::Report::BackgroundColor(rgb),
            )))) => {
                app.terminal_background = Some(rgb.map(|channel| (channel >> 8) as u8));
                Vec::new()
            }
            RuntimeEvent::Terminal(Some(Ok(_))) => Vec::new(),
            RuntimeEvent::Terminal(Some(Err(error))) => {
                runtime_error = Some(anyhow::Error::from(error).context("terminal input failed"));
                app.handle_action(termgram::input::KeyAction::Quit)
            }
            RuntimeEvent::Terminal(None) => app.handle_action(termgram::input::KeyAction::Quit),
            RuntimeEvent::Network(event) => {
                if let Some(event) = *event {
                    let refreshes_startup_warning =
                        matches!(&event, NetworkEvent::Auth(_) | NetworkEvent::Ready { .. });
                    let completes_startup =
                        matches!(&event, NetworkEvent::Status(ConnectionStatus::Online));
                    let outgoing = app.update(AppEvent::Network(event));
                    if completes_startup {
                        if let Some(warning) = startup_warning.take() {
                            app.handle_network(NetworkEvent::Error(warning));
                        }
                    } else if refreshes_startup_warning && let Some(warning) = &startup_warning {
                        app.handle_network(NetworkEvent::Error(warning.clone()));
                    }
                    outgoing
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
            RuntimeEvent::UpdateCheck { channel, result } => {
                update_check = None;
                if app.settings().automatic_update_checks
                    && app.settings().release_channel == channel
                {
                    match result {
                        Ok(Some(UpdateStatus::Available { version, .. })) => {
                            app.set_available_update(&version);
                        }
                        Ok(Some(UpdateStatus::UpToDate) | None) => app.clear_available_update(),
                        Err(_) => {}
                    }
                } else if app.settings().automatic_update_checks {
                    update_check = spawn_update_check(*app.settings());
                }
                Vec::new()
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
            RuntimeEvent::Preview(result) => {
                match result {
                    Ok(failures) => {
                        for failure in failures {
                            match failure.source {
                                termgram::media::MediaSource::Message {
                                    chat_id,
                                    message_id,
                                } => {
                                    app.media_preview_failed(chat_id, message_id, &failure.error);
                                }
                                termgram::media::MediaSource::File(path) => {
                                    app.attachment_draft.preview = false;
                                    app.status_message = Some(format!(
                                        "Preview {}: {}",
                                        path.display(),
                                        failure.error
                                    ));
                                }
                            }
                        }
                    }
                    Err(error) => app.status_message = Some(format!("{error:#}")),
                }
                Vec::new()
            }
            RuntimeEvent::DraftError(error) => {
                app.status_message = Some(error);
                Vec::new()
            }
            RuntimeEvent::Configuration(result) => {
                configuration_load = None;
                match result {
                    Ok(keymap) => {
                        if let Err(error) =
                            terminal.set_clipboard_enabled(keymap.attachments.terminal_clipboard)
                        {
                            let _ = terminal
                                .set_clipboard_enabled(app.keymap.attachments.terminal_clipboard);
                            app.configuration_failed(&format!(
                                "Could not apply terminal clipboard setting: {error}"
                            ));
                        } else {
                            clipboard.configuration_changed(&mut app);
                            app.install_configuration(keymap);
                        }
                    }
                    Err(error) => app.configuration_failed(&error),
                }
                Vec::new()
            }
            RuntimeEvent::Tick => app.update(AppEvent::Tick),
            RuntimeEvent::Notification(activity) => {
                notifications.activity(activity, &mut app);
                Vec::new()
            }
            RuntimeEvent::Clipboard(activity) => {
                clipboard.activity(&mut app, activity);
                Vec::new()
            }
        };

        let outgoing = clipboard.route(&mut app, outgoing);
        clipboard.flush(&mut app);
        let account_switch = dispatch(&mut app, &mut commands, &mut pending_commands, outgoing);
        if let Some(account) = account_switch {
            clipboard.cancel();
            if let Err(error) = draw_interface(&mut terminal, &mut preview, &mut app)
                .context("failed to draw account-switch feedback")
            {
                runtime_error = Some(error);
                break;
            }
            let Some(config) = base_config.as_ref() else {
                app.handle_network(NetworkEvent::Fatal(
                    "Telegram configuration is unavailable".to_owned(),
                ));
                continue;
            };
            if let Err(error) = restart_telegram_worker(
                config,
                account,
                &mut commands,
                &mut events,
                &mut worker,
                &mut pending_commands,
                &mut active_chat,
                &mut measurement,
            )
            .await
            {
                app.handle_network(NetworkEvent::Fatal(format!(
                    "Could not switch account: {error:#}"
                )));
            }
        }
        if let Some(active_chat) = &active_chat {
            let target = app.sync_target();
            active_chat.send_if_modified(|current| {
                if *current == target {
                    false
                } else {
                    *current = target;
                    true
                }
            });
        }
        synchronize_update_preferences(&mut app, &mut update_preferences, &mut update_check);
    }

    app.cancel_alerts();
    drop(preview);
    drop(terminal);
    if let Some(writer) = draft_writer {
        if let Some(snapshot) = app.take_draft_snapshot() {
            writer.queue(snapshot);
        }
        if let Err(error) = writer.finish().await {
            runtime_error = Some(error.context("could not save drafts before exit"));
        }
    }
    drop(commands.take());
    // Stop back-pressuring a worker that may be trying to report its final
    // status while the UI is no longer consuming network events.
    drop(events.take());
    if let Some(mut task) = worker.take()
        && timeout(Duration::from_secs(2), &mut task).await.is_err()
    {
        task.abort();
        drop(task.await);
    }

    runtime_error.map_or(Ok(()), Err)
}

// Polling previews and drawing frames both stay on the main event-loop thread.
fn draw_interface(
    terminal: &mut TerminalGuard,
    preview: &mut PreviewRenderer,
    app: &mut AppState,
) -> Result<()> {
    // Terminal::clear queries the cursor through Crossterm and would compete
    // with Yazi for input. Force cell updates without starting a second reader.
    let force = app.take_force_redraw();
    if force {
        preview.invalidate();
    }
    terminal.terminal_mut().draw(|frame| {
        ui::render(frame, app);
        preview.render(frame, app, force);
    })?;
    Ok(())
}

fn parse_arguments(mut arguments: impl Iterator<Item = String>) -> Result<CommandLine> {
    let Some(command) = arguments.next() else {
        return Ok(CommandLine::Tui);
    };
    match command.as_str() {
        "--version" | "-V" => {
            ensure_no_more_arguments(&mut arguments)?;
            Ok(CommandLine::Version)
        }
        "--help" | "-h" => {
            ensure_no_more_arguments(&mut arguments)?;
            Ok(CommandLine::Help)
        }
        "update" => {
            let mut channel = None;
            for argument in arguments.by_ref() {
                let selected = match argument.as_str() {
                    "--stable" => ReleaseChannel::Stable,
                    "--prerelease" => ReleaseChannel::Prerelease,
                    "--help" | "-h" => {
                        ensure_no_more_arguments(&mut arguments)?;
                        return Ok(CommandLine::Help);
                    }
                    _ => bail!("unknown update option {argument:?}; run `tg --help`"),
                };
                if channel.replace(selected).is_some() {
                    bail!("choose only one update channel");
                }
            }
            Ok(CommandLine::Update(channel))
        }
        _ => bail!("unknown argument {command:?}; run `tg --help`"),
    }
}

fn ensure_no_more_arguments(arguments: &mut impl Iterator<Item = String>) -> Result<()> {
    if let Some(argument) = arguments.next() {
        bail!("unexpected argument {argument:?}; run `tg --help`");
    }
    Ok(())
}

fn print_help() {
    println!(
        "Termgram — essential Telegram messaging in the terminal\n\n\
         Usage:\n  tg                         Open the TUI\n  \
         tg update [--stable|--prerelease]\n                             Install the newest release\n  \
         tg --version             Print the installed version\n  \
         tg --help                Show this help\n\n\
         Press a while navigating to switch or add Telegram accounts. F2 cycles\n\
         accounts and F3 adds one even from the sign-in screen. Press s to\n\
         configure update checks, release channel, and downloaded-file behavior."
    );
}

fn run_update_command(channel: Option<ReleaseChannel>) -> Result<()> {
    let channel = match channel {
        Some(channel) => channel,
        None => {
            Settings::load()
                .context("could not load the saved update channel; pass --stable or --prerelease")?
                .release_channel
        }
    };
    println!("Checking the {} channel…", channel.label().to_lowercase());
    match update::run(channel).context("Termgram update failed")? {
        UpdateOutcome::UpToDate => println!("tg {} is up to date", termgram::VERSION),
        UpdateOutcome::Installed { version } => println!("Updated tg to {version}"),
        UpdateOutcome::Staged { version, path } => println!(
            "Downloaded tg {version} to {}; replacement will finish after this process exits",
            path.display()
        ),
    }
    Ok(())
}

fn load_app_settings() -> (AppState, Settings) {
    let path = match Settings::path() {
        Ok(path) => path,
        Err(error) => {
            let settings = Settings {
                automatic_update_checks: false,
                ..Settings::default()
            };
            let mut app = AppState::with_ephemeral_settings(settings);
            app.handle_network(NetworkEvent::Error(format!(
                "Could not locate settings: {error:#}"
            )));
            return (app, settings);
        }
    };
    match Settings::load_from(&path) {
        Ok(settings) => (AppState::with_settings(settings, path), settings),
        Err(error) => {
            let settings = Settings {
                automatic_update_checks: false,
                ..Settings::default()
            };
            let mut app = AppState::with_settings(settings, path);
            app.handle_network(NetworkEvent::Error(format!(
                "Could not load settings: {error:#}"
            )));
            (app, settings)
        }
    }
}

fn spawn_update_check(settings: Settings) -> Option<UpdateCheck> {
    if !settings.automatic_update_checks {
        return None;
    }
    let (sender, receiver) = oneshot::channel();
    thread::Builder::new()
        .name("termgram-update-check".to_owned())
        .spawn(move || {
            let result = update::check_if_due(settings.release_channel)
                .map_err(|error| format!("{error:#}"));
            drop(sender.send(result));
        })
        .ok()?;
    Some(UpdateCheck {
        channel: settings.release_channel,
        receiver,
    })
}

fn synchronize_update_preferences(
    app: &mut AppState,
    previous: &mut Settings,
    update_check: &mut Option<UpdateCheck>,
) {
    let current = *app.settings();
    if current.automatic_update_checks != previous.automatic_update_checks
        || current.release_channel != previous.release_channel
    {
        if current.automatic_update_checks && update_check.is_none() {
            *update_check = spawn_update_check(current);
        }
        app.clear_available_update();
    }
    *previous = current;
}

async fn wait_for_update_check(
    update_check: &mut Option<UpdateCheck>,
) -> (ReleaseChannel, UpdateCheckResult) {
    let Some(update_check) = update_check else {
        return std::future::pending().await;
    };
    let result = (&mut update_check.receiver)
        .await
        .unwrap_or_else(|_| Err("update-check worker stopped unexpectedly".to_owned()));
    (update_check.channel, result)
}

async fn wait_for_animation_tick(enabled: bool) {
    if enabled {
        sleep(ANIMATION_INTERVAL).await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn wait_for_configuration(
    task: &mut Option<tokio::task::JoinHandle<Result<termgram::keymap::Keymap, String>>>,
) -> Result<termgram::keymap::Keymap, String> {
    match task {
        Some(task) => task
            .await
            .map_err(|e| format!("Configuration loader failed: {e}"))?,
        None => std::future::pending().await,
    }
}

async fn wait_for_draft_error(writer: &mut Option<termgram::drafts::Writer>) -> String {
    match writer {
        Some(writer) => writer.next_error().await,
        None => std::future::pending().await,
    }
}

fn dispatch(
    app: &mut AppState,
    commands: &mut Option<mpsc::Sender<TelegramCommand>>,
    pending: &mut VecDeque<TelegramCommand>,
    outgoing: Vec<TelegramCommand>,
) -> Option<u8> {
    let mut account_switch = None;
    for command in outgoing {
        let command = match command {
            TelegramCommand::SwitchAccount { account } => {
                account_switch = Some(account);
                pending.clear();
                continue;
            }
            command => command,
        };
        if pending.len() >= MAX_PENDING_COMMANDS {
            reject_overflow(app, command);
        } else {
            pending.push_back(command);
        }
    }
    if account_switch.is_some() {
        return account_switch;
    }
    let Some(sender) = commands.clone() else {
        pending.clear();
        return None;
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
    None
}

fn reject_overflow(app: &mut AppState, command: TelegramCommand) {
    if let Some(event) = command.failure("Telegram command queue is busy".to_owned()) {
        app.handle_network(event);
    }
}

#[allow(clippy::too_many_arguments)]
async fn restart_telegram_worker(
    base_config: &Config,
    account: u8,
    commands: &mut Option<mpsc::Sender<TelegramCommand>>,
    events: &mut Option<mpsc::Receiver<NetworkEvent>>,
    worker: &mut Option<tokio::task::JoinHandle<()>>,
    pending: &mut VecDeque<TelegramCommand>,
    active_chat: &mut Option<tokio::sync::watch::Sender<Option<termgram::model::ChatId>>>,
    measurement: &mut Option<tokio::sync::watch::Sender<bool>>,
) -> Result<()> {
    pending.clear();
    if let Some(sender) = commands.take() {
        drop(sender.try_send(TelegramCommand::Shutdown));
    }
    drop(events.take());
    if let Some(mut task) = worker.take()
        && timeout(Duration::from_secs(2), &mut task).await.is_err()
    {
        task.abort();
        drop(task.await);
    }

    let selected = base_config.for_account(account)?;
    let TelegramHandle {
        measure_latency,
        commands: next_commands,
        events: next_events,
        task,
        active_chat: next_active,
    } = telegram::spawn(selected);
    *measurement = Some(measure_latency);
    *active_chat = Some(next_active);
    *commands = Some(next_commands);
    *events = Some(next_events);
    *worker = Some(task);
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::{
        CommandLine, UpdateCheck, dispatch, parse_arguments, synchronize_update_preferences,
    };
    use termgram::{
        app::AppState,
        config::{ReleaseChannel, Settings},
        event::TelegramCommand,
    };

    fn parse(arguments: &[&str]) -> anyhow::Result<CommandLine> {
        parse_arguments(arguments.iter().map(ToString::to_string))
    }

    #[test]
    fn command_line_defaults_to_the_tui() {
        assert_eq!(parse(&[]).expect("command"), CommandLine::Tui);
    }

    #[test]
    fn command_line_selects_update_channels() {
        assert_eq!(
            parse(&["update"]).expect("saved channel"),
            CommandLine::Update(None)
        );
        assert_eq!(
            parse(&["update", "--stable"]).expect("stable"),
            CommandLine::Update(Some(ReleaseChannel::Stable))
        );
        assert_eq!(
            parse(&["update", "--prerelease"]).expect("prerelease"),
            CommandLine::Update(Some(ReleaseChannel::Prerelease))
        );
    }

    #[test]
    fn command_line_rejects_unknown_or_ambiguous_arguments() {
        assert!(parse(&["wat"]).is_err());
        assert!(parse(&["--version", "extra"]).is_err());
        assert!(parse(&["update", "--stable", "--prerelease"]).is_err());
        assert!(parse(&["update", "--help", "extra"]).is_err());
    }

    #[test]
    fn disabling_update_checks_ignores_inflight_result_and_clears_the_hint() {
        let settings = Settings {
            automatic_update_checks: false,
            ..Settings::default()
        };
        let mut app = AppState::with_settings(
            settings,
            std::env::temp_dir().join("unused-termgram-settings"),
        );
        app.set_available_update("0.1.9");
        let mut previous = Settings::default();
        let (_sender, receiver) = tokio::sync::oneshot::channel();
        let mut receiver = Some(UpdateCheck {
            channel: ReleaseChannel::Stable,
            receiver,
        });

        synchronize_update_preferences(&mut app, &mut previous, &mut receiver);

        assert!(receiver.is_some());
        assert_eq!(previous, settings);
        assert_eq!(app.available_update(), None);
    }

    #[test]
    fn account_switch_commands_are_kept_out_of_the_telegram_queue() {
        let mut app = AppState::new();
        let mut commands = None;
        let mut pending = VecDeque::from([TelegramCommand::RefreshDialogs]);
        let selected = dispatch(
            &mut app,
            &mut commands,
            &mut pending,
            vec![TelegramCommand::SwitchAccount { account: 3 }],
        );

        assert_eq!(selected, Some(3));
        assert!(pending.is_empty());
    }
}
