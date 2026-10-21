use super::modal::centered_rect;
use crate::app::{AppMode, AppState};
use crate::client::ItemType;
use crate::ui::colors::Theme;
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

pub(super) fn render_header(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let title = match state.navigation.current() {
        Some(level) => format!("Hopper - {}", level.title),
        None => "Hopper".to_string(),
    };
    let mut spans = vec![Span::styled(
        title,
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD),
    )];
    if state.client.is_mock() {
        spans.push(Span::styled("  [demo]", Style::default().fg(theme.dim)));
    }
    if state.delete_danger_mode {
        spans.push(Span::styled(
            "  [DANGER MODE: deletes skip confirmation]",
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let header = Paragraph::new(Line::from(spans)).alignment(Alignment::Center);
    frame.render_widget(header, area);
}

pub(super) fn render_status_bar(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let mut text = if let Some(msg) = state.busy_message() {
        const SPINNER: &[char] = &['|', '/', '-', '\\'];
        let icon = SPINNER[state.spinner_index % SPINNER.len()];
        format!("{} {}", icon, msg)
    } else if state.mode == AppMode::Search {
        format!("/{}", state.search_query)
    } else if let Some(msg) = &state.status_message {
        msg.clone()
    } else {
        state.get_current_path()
    };
    if !state.selected_items.is_empty() {
        text.push_str(&format!("  |  {} marked", state.selected_items.len()));
    }
    if !state.cut_items.is_empty() {
        text.push_str(&format!("  |  {} cut (p to paste)", state.cut_items.len()));
    }

    let status = Paragraph::new(text)
        .style(Style::default().fg(theme.foreground).bg(theme.panel))
        .alignment(Alignment::Left);

    frame.render_widget(status, area);
}

pub(super) fn render_footer(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let help_text = match state.mode {
        AppMode::Help => "Esc/?: close help  ↑/↓: scroll  PgUp/PgDn: page",
        AppMode::Search => "Type to filter  Enter: open  Esc: cancel  ↑/↓: move",
        AppMode::Input => "Enter: confirm  Esc: cancel",
        AppMode::BucketForm => "Tab/↑/↓: field  Type: edit  Enter: save  Esc: cancel",
        AppMode::DeleteConfirm => "Enter/y: delete  Esc/n: cancel  !: toggle danger mode",
        AppMode::DeleteDangerConfirm => "Enter/y: enable danger mode  Esc/n: keep confirmations",
        AppMode::Normal => {
            if state.active_location().is_none() {
                "?: help  q: quit  /: filter  a: add bucket  d: remove from config  j/k: move  h/l: pane  Enter: open"
            } else {
                "?: help  q: quit  /: filter  D/U: download/upload  d: delete  y: copy URI  j/k: move  h/l: pane  Enter: open  Backspace: back"
            }
        }
    };

    let footer = Paragraph::new(help_text)
        .style(Style::default().fg(theme.dim).bg(theme.background))
        .alignment(Alignment::Center);

    frame.render_widget(footer, area);
}

pub(super) fn render_help(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let at_root = state.active_location().is_none();
    let selected_type = state.selected_item().map(|item| item.item_type);

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let mut help_text = vec![
        Line::from(Span::styled(
            "Hopper - Keyboard Shortcuts",
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled("Navigation:", bold)),
        Line::from("  j/↓        Move down"),
        Line::from("  k/↑        Move up"),
        Line::from("  h/←        Move left / slide window left"),
        Line::from("  l/→        Move right / slide window right"),
        Line::from("  Home/Ctrl+g Go to top"),
        Line::from("  G          Go to bottom"),
        Line::from("  Ctrl+u     Page up"),
        Line::from("  Ctrl+d     Page down"),
        Line::from("  Tab        Next pane"),
        Line::from("  BackTab    Previous pane"),
        Line::from(""),
        Line::from(Span::styled("Actions:", bold)),
        Line::from("  Enter        Open bucket/prefix, load more, or edit object in $EDITOR"),
        Line::from("  Space        Toggle selection"),
        Line::from("  Backspace    Go back"),
        Line::from("  R            Refresh current level"),
        Line::from("  /            Start search/filter"),
        Line::from("  y            Copy s3:// URI to clipboard"),
        Line::from("  o            Open in the AWS console"),
        Line::from("  z            Compute total size of bucket/prefix"),
        Line::from("  D            Download selected items"),
        Line::from("  [ / ]        Scroll details pane"),
    ];
    if at_root {
        help_text.extend([
            Line::from("  a            Add a bucket to your config (bucket, profile, prefix)"),
            Line::from("  d            Remove selected buckets from your config"),
        ]);
    } else {
        help_text.extend([
            Line::from("  U            Upload a local file or directory here"),
            Line::from("  a            Create object (opens $EDITOR) or folder (name ending in /)"),
            Line::from("  r            Rename selected object or prefix"),
            Line::from("  d            Delete selected items (prefixes recursively)"),
            Line::from("  x            Cut selected items"),
            Line::from("  p            Paste (move) cut items here"),
        ]);
    }
    help_text.push(Line::from("  !            Toggle danger delete mode"));
    if selected_type == Some(ItemType::Object) {
        help_text.push(Line::from(
            "  Enter        Edit object in $EDITOR (uploaded back only if changed)",
        ));
        help_text.push(Line::from(
            "  v            View object in a [[viewers]] program (downloaded to a temp file)",
        ));
    }
    help_text.extend([
        Line::from("  ?            Toggle this help"),
        Line::from("  q            Quit"),
        Line::from(""),
        Line::from(Span::styled("Configuration:", bold)),
    ]);
    if state.config.loaded_files.is_empty() {
        help_text.push(Line::from("  No config file loaded."));
    }
    for path in &state.config.loaded_files {
        help_text.push(Line::from(format!("  Loaded {}", path.display())));
    }
    if let Some(path) = &state.config_write_path {
        help_text.push(Line::from(format!(
            "  New buckets are saved to {}",
            path.display()
        )));
    }
    help_text.extend([
        Line::from(""),
        Line::from(Span::styled(
            "Esc/? closes. Use \u{2191}/\u{2193} or PgUp/PgDn to scroll.",
            Style::default().fg(theme.warning),
        )),
    ]);

    let total_lines = help_text.len() as u16;
    let desired_height = total_lines.saturating_add(2);
    let max_height = area.height.saturating_sub(2);
    let help_height = desired_height.min(max_height.max(10));
    let visible_lines = help_height.saturating_sub(2) as usize;
    let max_scroll = (total_lines as usize).saturating_sub(visible_lines);
    let scroll = state.help_scroll.min(max_scroll);

    let help = Paragraph::new(help_text)
        .style(Style::default().fg(theme.foreground).bg(theme.surface))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.primary))
                .title(" Help "),
        )
        .alignment(Alignment::Left)
        .scroll((scroll as u16, 0));

    // Center the help dialog with dynamic height so context-specific shortcuts are not clipped.
    let help_area = centered_rect(area, 84, help_height);
    frame.render_widget(help, help_area);
}
