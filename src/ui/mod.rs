use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, Gauge, Paragraph, Row, Table, TableState, Wrap,
    },
    Frame,
};

use crate::{
    app::{
        format_elapsed, App, AppMode, ArchiveView, BrowserView, BuiltinTool, ClipboardMode,
        DeviceView, NetworkView, PartitionOverlay, PartitionView, Prompt, SearchForm, SearchView,
        ToolsView, TrashView,
    },
    entry::{human_size, EntryKind},
    icons::Icons,
    partition::Filesystem,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const ACCENT: Color = Color::Gray;
const MUTED: Color = Color::DarkGray;

mod browser;
mod chrome;
mod dialogs;
mod search;
mod storage;
mod tools;

use browser::*;
use chrome::*;
use dialogs::*;
use search::*;
use storage::*;
use tools::*;

pub fn draw(frame: &mut Frame, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(status_bar_height(app)),
            Constraint::Length(shortcut_bar_height(app, frame.area().width)),
        ])
        .split(frame.area());

    draw_header(frame, app, rows[0]);
    draw_browser(frame, app, rows[1]);

    match &app.mode {
        AppMode::Browser => {}
        AppMode::Archive(view) => draw_archive(frame, app, view),
        AppMode::Tools(view) => draw_tools(frame, app, view),
        AppMode::Prompt(prompt) => draw_prompt(frame, app, prompt),
        AppMode::Progress => draw_progress_modal(frame, app),
        AppMode::SearchProgress => draw_search_progress(frame, app),
        AppMode::SearchForm(form) => draw_search_form(frame, app, form),
        AppMode::SearchResults => {
            if let Some(view) = &app.search_results {
                draw_search_results(frame, app, view);
            }
        }
        AppMode::UpdateProgress => draw_update_progress(frame),
        AppMode::Trash(view) => draw_trash(frame, app, view),
        AppMode::Devices(view) => draw_devices(frame, app, view),
        AppMode::Network(view) => draw_network(frame, app, view),
        AppMode::NetworkProgress => draw_network_progress(frame),
        AppMode::Partitions(view) => draw_partitions(frame, app, view),
        AppMode::Help => draw_help(frame, app),
        AppMode::Info(entry) => draw_info(frame, app, entry.as_ref()),
        AppMode::ConfigError { path, error } => draw_config_error(frame, app, path, error),
    }

    // Keep these bars above app panels so the active context's shortcuts are
    // always visible, including when a panel uses most of the terminal.
    draw_status(frame, app, rows[2]);
    draw_shortcuts(frame, app, rows[3]);
}

#[cfg(test)]
mod tests;
