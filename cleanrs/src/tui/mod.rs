//! Interactive browser and cleanup flow through the shared `pipeline`.
mod app;
mod cache;
mod event;
mod keymap;
#[cfg(test)]
mod tests;
mod theme;
mod views;
mod worker;

use anyhow::{Context, Result, bail};
use app::App;
use crossterm::{
    cursor::Show,
    event::{self as terminal_event, Event as TerminalEvent, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use event::Event;
use keymap::Keymap;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, IsTerminal};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use theme::Theme;
use worker::Workers;

type PanicHook = Arc<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}
struct TerminalGuard {
    previous_hook: PanicHook,
}
impl TerminalGuard {
    fn enter() -> Result<Self> {
        let previous_hook: PanicHook = std::panic::take_hook().into();
        let hook = Arc::clone(&previous_hook);
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            hook(info);
        }));
        // Construct the guard before the first fallible mode change, so partial
        // setup failures also restore both terminal state and the old hook.
        let guard = Self { previous_hook };
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(guard)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
        if !std::thread::panicking() {
            let previous = Arc::clone(&self.previous_hook);
            std::panic::set_hook(Box::new(move |info| previous(info)));
        }
    }
}

struct Input {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    events: crossbeam_channel::Receiver<Event>,
}
impl Input {
    fn start() -> Self {
        let (sender, events) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let result = (|| -> io::Result<Option<Event>> {
                    if !terminal_event::poll(Duration::from_millis(100))? {
                        return Ok(None);
                    }
                    Ok(match terminal_event::read()? {
                        TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => {
                            Some(Event::Key(key))
                        }
                        TerminalEvent::Resize(_, _) => Some(Event::Resize),
                        _ => None,
                    })
                })();
                match result {
                    Ok(Some(event)) => {
                        if sender.send(event).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = sender.send(Event::InputError(error.to_string()));
                        break;
                    }
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
            events,
        }
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn run(path: &Path, verbose: u8) -> Result<()> {
    // Validate both streams before any terminal modes or filesystem traversal.
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("cleanrs tui requires a terminal on both stdin and stdout");
    }
    let path = path
        .canonicalize()
        .with_context(|| format!("cannot open {}", path.display()))?;
    if !path.is_dir() {
        bail!("TUI path must be a directory: {}", path.display());
    }
    // File logging keeps diagnostics available without corrupting raw-mode
    // rendering. Use the same fixed state location as history, never the cwd.
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let journal = crate::pipeline::journal_for_environment(home.as_deref())?;
    let log_path = journal.path().with_file_name("tui.log");
    std::fs::create_dir_all(log_path.parent().context("TUI log parent")?)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let log = options.open(log_path).context("cannot open TUI log")?;
    let filter = match verbose {
        0 => "warn",
        1 => "cleanrs=info,cleanrs_core=info",
        _ => "cleanrs=debug,cleanrs_core=debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()),
        )
        .with_writer(std::sync::Mutex::new(log))
        .with_ansi(false)
        .try_init()
        .ok();
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let workers = Workers::start_with_cache(cache::DiskCache::for_environment(home.as_deref()));
    let input = Input::start();
    let mut app = App::new(path);
    app.home = home;
    let theme = Theme::detect();
    let mut keymap = Keymap::default();
    let ticks = crossbeam_channel::tick(Duration::from_millis(80));
    let freshness_ticks = crossbeam_channel::tick(Duration::from_secs(1));
    while !app.quit {
        for request in app.drain_requests() {
            workers
                .requests
                .send(request)
                .context("TUI worker stopped")?;
        }
        let size = terminal.size()?;
        let screen = app
            .screens
            .iter()
            .rev()
            .find(|s| **s != app::Screen::Help)
            .copied()
            .unwrap_or(app::Screen::Browser);
        let overhead = if app.screen() == app::Screen::Help {
            2
        } else {
            match screen {
                app::Screen::Browser => {
                    if app.info {
                        15
                    } else {
                        7
                    }
                }
                app::Screen::Targets => {
                    if size.width < 100 {
                        15
                    } else {
                        14
                    }
                }
                app::Screen::Review => 11,
                app::Screen::Run => size.height.saturating_mul(55) / 100,
                _ => 8,
            }
        };
        app.page_width = size.width;
        app.page_rows = size.height.saturating_sub(overhead).max(1) as usize;
        terminal.draw(|frame| views::render(frame, &app, theme))?;
        // Busy frames animate at 12.5 Hz; idle freshness updates once per
        // second so old data visibly becomes stale without keyboard input.
        let tick = if app.busy() {
            ticks.clone()
        } else {
            freshness_ticks.clone()
        };
        let event = crossbeam_channel::select! {
            recv(input.events) -> event => event.context("terminal input stopped")?,
            recv(workers.events) -> event => Event::Worker(event.context("TUI worker stopped")?),
            recv(tick) -> _ => Event::Tick,
        };
        match event {
            Event::Key(key) => {
                app.status = None;
                let action = if app.confirm_quit {
                    keymap.quit_action(key)
                } else {
                    keymap.action(key, app.screen(), app.filtering)
                };
                if let Some(action) = action {
                    app.handle(action);
                }
            }
            Event::Tick => app.on_tick(),
            Event::Resize => {}
            Event::Worker(event) => app.on_worker(event),
            Event::InputError(error) => bail!("terminal input failed: {error}"),
        }
    }
    for request in app.drain_requests() {
        let _ = workers.requests.send(request);
    }
    Ok(())
}
