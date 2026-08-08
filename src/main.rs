mod app;
mod config;
mod entry;
mod error;
mod luks;
mod operation;
mod safety;
mod trash;
mod ui;

use std::{
    env,
    io::{self, stdout},
    path::PathBuf,
    time::Duration,
};

use app::{App, PendingSystemAction, SystemActionOutcome};
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

    while app.running {
        app.poll_operation();
        app.poll_luks_operation();
        app.poll_devices();
        terminal.draw(|frame| ui::draw(frame, &app))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => app.handle_key(key),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
        if let Some(action) = app.take_system_action() {
            terminal.show_cursor()?;
            disable_raw_mode()?;
            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
            let result = execute_system_action(&action);
            enable_raw_mode()?;
            execute!(terminal.backend_mut(), EnterAlternateScreen)?;
            terminal.clear()?;
            terminal.hide_cursor()?;
            app.finish_system_action(&action, result);
        }
    }
    Ok(())
}

fn execute_system_action(action: &PendingSystemAction) -> error::Result<SystemActionOutcome> {
    match action {
        PendingSystemAction::Editor {
            program,
            path,
            reload_config,
        } => {
            let status = std::process::Command::new(program)
                .arg(path)
                .status()
                .map_err(|source| error::io_error(format!("could not run {program}"), source))?;
            if !status.success() {
                return Err(error::MinfmError::Message(format!(
                    "{program} exited with status {status}"
                )));
            }
            Ok(SystemActionOutcome::EditorFinished {
                reload_config: *reload_config,
                message: format!("Editor closed: {}", path.display()),
            })
        }
    }
}

fn parse_args() -> Result<(bool, PathBuf), Box<dyn std::error::Error>> {
    let mut force_read_only = false;
    let mut path = None;
    for argument in env::args_os().skip(1) {
        if argument == "--read-only" {
            force_read_only = true;
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
