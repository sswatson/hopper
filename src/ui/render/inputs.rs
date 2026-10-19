use super::modal::{centered_rect, selection_row_style};
use crate::app::{AppState, BUCKET_FORM_FIELDS, InputPurpose};
use crate::ui::colors::Theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

fn prompt_for(purpose: &InputPurpose) -> String {
    match purpose {
        InputPurpose::Rename { item } => format!("New name for {}:", item.full_path()),
        InputPurpose::Download { items } => {
            if items.len() == 1 {
                format!("Download {} into local directory:", items[0].full_path())
            } else {
                format!("Download {} items into local directory:", items.len())
            }
        }
        InputPurpose::Upload { target } => {
            format!("Local file or directory to upload into {}:", target.uri())
        }
        InputPurpose::Create { target } => format!(
            "New object in {} (end with / for a folder; otherwise $EDITOR opens):",
            target.uri()
        ),
    }
}

pub(super) fn render_text_input(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let Some(input) = &state.text_input else {
        return;
    };
    let mut lines = vec![
        Line::from(prompt_for(&input.purpose)),
        Line::from(""),
        Line::from(vec![
            Span::styled("> ", Style::default().fg(theme.primary)),
            Span::raw(input.buffer.clone()),
            Span::styled("█", Style::default().fg(theme.primary)),
        ]),
    ];
    if let Some(hint) = &input.hint {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            hint.clone(),
            Style::default().fg(theme.dim),
        )));
    }
    lines.push(Line::from(""));
    let keys = if input.purpose.is_local_path() {
        "Enter: confirm  Tab: complete path  Ctrl+w: delete segment  Esc: cancel"
    } else {
        "Enter: confirm  Ctrl+u: clear  Esc: cancel"
    };
    lines.push(Line::from(Span::styled(
        keys,
        Style::default().fg(theme.dim),
    )));

    let width = area.width.saturating_sub(8).clamp(40, 100);
    let height = lines.len() as u16 + 2 + 1;
    let dialog = Paragraph::new(lines)
        .style(Style::default().fg(theme.foreground).bg(theme.surface))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.primary))
                .title(input.purpose.title()),
        )
        .wrap(Wrap { trim: false });
    let dialog_area = centered_rect(area, width, height);
    frame.render_widget(Clear, dialog_area);
    frame.render_widget(dialog, dialog_area);
}

pub(super) fn render_bucket_form(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let form = &state.bucket_form;
    let placeholders = [
        "bucket name or s3://bucket/prefix",
        match &state.config.default_profile {
            Some(profile) => profile.as_str(),
            None => "default",
        },
        "(bucket root)",
        "(detect automatically)",
        "(bucket name)",
    ];

    let target = state
        .config_write_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "(no config path available)".to_string());
    let mut lines = vec![
        Line::from(format!("Add a bucket to {}", target)),
        Line::from(""),
    ];
    for (index, label) in BUCKET_FORM_FIELDS.iter().enumerate() {
        let is_selected = index == form.selected;
        let value = &form.values[index];
        let marker = if is_selected { "> " } else { "  " };
        let mut spans = vec![Span::styled(
            format!("{}{:<9}", marker, label),
            selection_row_style(theme, is_selected),
        )];
        if value.is_empty() && !is_selected {
            spans.push(Span::styled(
                placeholders[index],
                Style::default().fg(theme.dim),
            ));
        } else {
            spans.push(Span::raw(value.clone()));
        }
        if is_selected {
            spans.push(Span::styled("█", Style::default().fg(theme.primary)));
        }
        lines.push(Line::from(spans));
    }
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "Profile is the AWS profile whose credentials can read the bucket.",
            Style::default().fg(theme.dim),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Tab/↓: next field  Shift+Tab/↑: previous  Enter: save  Esc: cancel",
            Style::default().fg(theme.dim),
        )),
    ]);

    let height = lines.len() as u16 + 2;
    let dialog = Paragraph::new(lines)
        .style(Style::default().fg(theme.foreground).bg(theme.surface))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.primary))
                .title(" Add Bucket "),
        )
        .wrap(Wrap { trim: false });
    let dialog_area = centered_rect(area, 76, height);
    frame.render_widget(Clear, dialog_area);
    frame.render_widget(dialog, dialog_area);
}
