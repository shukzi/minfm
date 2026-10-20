use std::{
    io::{self, stdout},
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::{app::App, cli::Options, config, launcher, ui};

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
}

pub fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let load = config::load();
    let mut app = App::new(options.start, load, options.force_read_only);

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut redraw = true;
    let mut last_animation = Instant::now();
    while app.running {
        redraw |= poll_background(&mut app);
        if app.needs_animation() && last_animation.elapsed() >= Duration::from_millis(180) {
            redraw = true;
            last_animation = Instant::now();
        }
        if redraw {
            terminal.draw(|frame| ui::draw(frame, &app))?;
            redraw = false;
        }
        if event::poll(input_poll(&app))? {
            match event::read()? {
                Event::Key(key) => {
                    app.handle_key(key);
                    redraw = true;
                }
                Event::Resize(_, _) => redraw = true,
                _ => {}
            }
        }
        if let Some(action) = app.take_terminal_editor() {
            terminal.show_cursor()?;
            disable_raw_mode()?;
            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
            let result = launcher::run_terminal_editor(action.program(), action.path());
            enable_raw_mode()?;
            execute!(terminal.backend_mut(), EnterAlternateScreen)?;
            terminal.clear()?;
            terminal.hide_cursor()?;
            app.finish_terminal_editor(&action, result);
            redraw = true;
        }
    }
    Ok(())
}

#[inline]
fn input_poll(app: &App) -> Duration {
    if app.browser_loading
        || app.device_refreshing
        || app.network_refreshing
        || app.partition_refreshing
        || app.search_running()
    {
        Duration::from_millis(16)
    } else {
        Duration::from_millis(100)
    }
}

#[inline]
fn poll_background(app: &mut App) -> bool {
    app.poll_browser_load()
        | app.poll_operation()
        | app.poll_archive()
        | app.poll_luks_operation()
        | app.poll_search()
        | app.poll_update()
        | app.poll_devices()
        | app.poll_network()
        | app.poll_partitions()
        | app.poll_partition_operation()
        | app.poll_file_launch()
        | app.poll_status_expiry()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ConfigLoad};

    fn app(root: &std::path::Path) -> App {
        App::new(
            root.to_path_buf(),
            ConfigLoad::Valid {
                config: Config::default(),
                path: root.join("config.toml"),
            },
            false,
        )
    }

    #[test]
    fn idle_and_busy_poll_intervals_preserve_reference_behavior() {
        let root = tempfile::tempdir().unwrap();
        let mut app = app(root.path());
        app.browser_loading = false;
        app.device_refreshing = false;
        app.network_refreshing = false;
        app.partition_refreshing = false;
        assert_eq!(input_poll(&app), Duration::from_millis(100));
        app.browser_loading = true;
        assert_eq!(input_poll(&app), Duration::from_millis(16));
    }
}
