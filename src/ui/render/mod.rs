mod chrome;
mod confirm;
mod inputs;
mod modal;
mod panes;

use crate::app::{AppMode, AppState};
use crate::ui::colors::Theme;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
};

pub fn render(frame: &mut Frame, state: &AppState) {
    let theme = Theme::hopper_dark();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Header
            Constraint::Min(0),    // Main content
            Constraint::Length(1), // Status bar
            Constraint::Length(1), // Footer
        ])
        .split(frame.area());

    // Render header
    chrome::render_header(frame, chunks[0], state, &theme);

    // Every mode draws the miller columns, with a modal on top for the
    // non-browsing modes. The match is exhaustive so adding an AppMode
    // variant is a compile error here rather than a silent fallthrough.
    match state.mode {
        AppMode::Help => {
            chrome::render_help(frame, chunks[1], state, &theme);
        }
        AppMode::Input => {
            panes::render_miller_columns(frame, chunks[1], state, &theme);
            inputs::render_text_input(frame, chunks[1], state, &theme);
        }
        AppMode::BucketForm => {
            panes::render_miller_columns(frame, chunks[1], state, &theme);
            inputs::render_bucket_form(frame, chunks[1], state, &theme);
        }
        AppMode::DeleteConfirm => {
            panes::render_miller_columns(frame, chunks[1], state, &theme);
            confirm::render_delete_confirm(frame, chunks[1], state, &theme);
        }
        AppMode::DeleteDangerConfirm => {
            panes::render_miller_columns(frame, chunks[1], state, &theme);
            confirm::render_delete_danger_confirm(frame, chunks[1], &theme);
        }
        AppMode::Normal | AppMode::Search => {
            panes::render_miller_columns(frame, chunks[1], state, &theme);
        }
    }

    // Render status bar
    chrome::render_status_bar(frame, chunks[2], state, &theme);

    // Render footer
    chrome::render_footer(frame, chunks[3], state, &theme);
}
