use super::modal::centered_rect;
use crate::app::{AppState, DeleteTarget};
use crate::client::ItemType;
use crate::ui::colors::Theme;
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::Style,
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

pub(super) fn render_delete_confirm(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
) {
    let Some(target) = &state.delete_target else {
        return;
    };

    let (title, intro, items, consequence) = match target {
        DeleteTarget::Items(items) => {
            let has_prefix = items.iter().any(|item| item.item_type == ItemType::Prefix);
            (
                " Confirm Delete ",
                "You are about to permanently delete from S3:",
                items,
                if has_prefix {
                    "Prefixes are deleted recursively (every object under them). This cannot be undone."
                } else {
                    "This action cannot be undone."
                },
            )
        }
        DeleteTarget::ConfigEntries(items) => (
            " Remove From Config ",
            "Remove these buckets from your hopper config?",
            items,
            "Only the config entry is removed; the buckets and their data are untouched.",
        ),
    };

    let mut lines = vec![Line::from(intro), Line::from("")];
    const SHOWN: usize = 6;
    for item in items.iter().take(SHOWN) {
        lines.push(Line::from(format!("  {}", item.full_path())));
    }
    if items.len() > SHOWN {
        lines.push(Line::from(format!(
            "  ... and {} more",
            items.len() - SHOWN
        )));
    }
    lines.extend([
        Line::from(""),
        Line::from(consequence),
        Line::from(format!(
            "Danger mode: {}",
            if state.delete_danger_mode {
                "ON"
            } else {
                "OFF"
            }
        )),
        Line::from(""),
        Line::from("Press Enter or y to confirm."),
        Line::from("Press Esc or n to cancel."),
        Line::from(if state.delete_danger_mode {
            "Press ! to disable danger mode."
        } else {
            "Press ! to enable danger mode (skip this dialog)."
        }),
    ]);

    let height = lines.len() as u16 + 2;
    let dialog = Paragraph::new(lines)
        .style(Style::default().fg(theme.foreground).bg(theme.surface))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.warning))
                .title(title),
        )
        .wrap(Wrap { trim: false })
        .alignment(Alignment::Left);

    let dialog_area = centered_rect(area, 80, height);
    frame.render_widget(Clear, dialog_area);
    frame.render_widget(dialog, dialog_area);
}

pub(super) fn render_delete_danger_confirm(frame: &mut Frame, area: Rect, theme: &Theme) {
    let lines = vec![
        Line::from("Enable danger mode?"),
        Line::from(""),
        Line::from("Deletes will happen immediately, without a confirmation"),
        Line::from("dialog, until you press ! again or quit."),
        Line::from(""),
        Line::from("Press Enter or y to enable."),
        Line::from("Press Esc or n to keep confirmations."),
    ];
    let dialog = Paragraph::new(lines)
        .style(Style::default().fg(theme.foreground).bg(theme.surface))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.error))
                .title(" Danger Mode "),
        );
    let dialog_area = centered_rect(area, 62, 9);
    frame.render_widget(Clear, dialog_area);
    frame.render_widget(dialog, dialog_area);
}
