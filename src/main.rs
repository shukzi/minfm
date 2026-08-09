mod app;
mod browser_loader;
mod config;
mod entry;
mod error;
mod launcher;
mod luks;
mod operation;
mod safety;
mod trash;
mod ui;
mod updater;

use std::{
    env,
    io::{self, stdout},
    path::PathBuf,
    time::{Duration, Instant},
};

use app::App;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (force_read_only, start) = parse_args()?;
    let load = config::load();
    let mut app = App::new(start, load, force_read_only);

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut redraw = true;
    let mut last_animation = Instant::now();
    while app.running {
        redraw |= app.poll_browser_load();
        redraw |= app.poll_operation();
        redraw |= app.poll_luks_operation();
        redraw |= app.poll_search();
        redraw |= app.poll_update();
        redraw |= app.poll_devices();
        redraw |= app.poll_file_launch();
        redraw |= app.poll_status_expiry();
        if app.needs_animation() && last_animation.elapsed() >= Duration::from_millis(180) {
            redraw = true;
            last_animation = Instant::now();
        }
        if redraw {
            terminal.draw(|frame| ui::draw(frame, &app))?;
            redraw = false;
        }
        let input_poll = if app.browser_loading || app.device_refreshing {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(100)
        };
        if event::poll(input_poll)? {
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

fn parse_args() -> Result<(bool, PathBuf), Box<dyn std::error::Error>> {
    let mut force_read_only = false;
    let mut path = None;
    for argument in env::args_os().skip(1) {
        if argument == "--read-only" {
            force_read_only = true;
        } else if argument == "--version" || argument == "-V" {
            println!("minfm {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        } else if argument == "--help" || argument == "-h" {
            println!("minfm [--read-only] [path]");
            std::process::exit(0);
        } else if path.is_none() {
            path = Some(PathBuf::from(argument));
        } else {
            return Err("only one starting path may be provided".into());
        }
    }
    let path = path.unwrap_or(env::current_dir()?);
    if !path.is_dir() {
        return Err(format!("not a directory: {}", path.display()).into());
    }
    Ok((force_read_only, path.canonicalize()?))
}
