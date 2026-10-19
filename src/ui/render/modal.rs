use crate::ui::colors::Theme;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
};

/// Centers a dialog of at most `width` x `height` in `area`, clamping the
/// dialog to the available space.
pub(super) fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

/// Standard style for rows in modal lists: highlighted with the selection
/// background when selected, plain foreground otherwise.
pub(super) fn selection_row_style(theme: &Theme, is_selected: bool) -> Style {
    if is_selected {
        Style::default()
            .fg(theme.foreground)
            .bg(theme.selection)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.foreground)
    }
}
