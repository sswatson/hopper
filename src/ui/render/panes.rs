use crate::app::{AppState, ListState};
use crate::client::{ItemType, S3Item};
use crate::ui::colors::{Theme, bucket_style, item_type_style, storage_class_style};
use crate::ui::helpers::{format_datetime_short, format_size};
use crate::ui::preview::render_preview_pane;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use std::collections::HashSet;

pub(super) fn render_miller_columns(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
) {
    let pane_count = state.list_states.len();

    if pane_count == 0 {
        return;
    }

    // Split area into navigation panes (left) and preview pane (right)
    // Preview pane gets 40% of the width
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(60), // Navigation panes
            Constraint::Percentage(40), // Preview pane
        ])
        .split(area);

    // Size each navigation pane proportionally to its longest item name.
    // Using Fill(weight) distributes space in proportion to content width,
    // so short-content panes get less space but surplus is still allocated
    // rather than left as dead space.
    // Padding: 2 (borders) + 3 (icon + space) + 1 (breathing room) = 6
    let nav_constraints: Vec<Constraint> = state
        .list_states
        .iter()
        .map(|ls| {
            use unicode_width::UnicodeWidthStr;
            let max_row = ls
                .filtered_items
                .iter()
                .map(|&idx| {
                    let item = &ls.items[idx];
                    item.name.as_str().width() + hint_width(item)
                })
                .max()
                .unwrap_or(0);
            Constraint::Fill((max_row + 6).max(1) as u16)
        })
        .collect();

    let nav_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(nav_constraints)
        .split(main_chunks[0]);

    let cut_paths: HashSet<String> = state.cut_items.iter().map(S3Item::full_path).collect();
    let show_gutter = !state.selected_items.is_empty() || !cut_paths.is_empty();

    // Render each navigation pane
    for (i, list_state) in state.list_states.iter().enumerate() {
        if i < nav_chunks.len() {
            render_pane(
                frame,
                nav_chunks[i],
                state,
                list_state,
                i == state.active_pane,
                &cut_paths,
                show_gutter,
                theme,
            );
        }
    }

    // Render preview pane
    render_preview_pane(frame, main_chunks[1], state, theme);
}

/// Approximate width of the dim hint after a name (size, profile tag), so
/// panes are sized to show it rather than clip it.
fn hint_width(item: &S3Item) -> usize {
    match item.item_type {
        ItemType::Object => item
            .size
            .map(|size| format_size(size).len() + 2)
            .unwrap_or(0),
        ItemType::Bucket => item
            .bucket
            .profile
            .as_ref()
            .map(|p| p.len() + 4)
            .unwrap_or(0),
        ItemType::Prefix | ItemType::LoadMore => 0,
    }
}

#[allow(clippy::too_many_arguments)]
fn render_pane(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    list_state: &ListState,
    is_active: bool,
    cut_paths: &HashSet<String>,
    show_gutter: bool,
    theme: &Theme,
) {
    let border_style = if is_active {
        Style::default().fg(theme.primary)
    } else {
        Style::default().fg(theme.panel)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .style(Style::default().bg(theme.surface));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if list_state.is_empty() {
        let message = if list_state.items.is_empty() {
            "(empty)"
        } else {
            "(no matches)"
        };
        let empty_msg = Paragraph::new(message)
            .style(Style::default().fg(theme.dim))
            .alignment(Alignment::Center);
        frame.render_widget(empty_msg, inner);
        return;
    }

    // Calculate scroll offset
    let visible_height = (inner.height as usize).max(1);
    let mut scroll_offset = list_state.scroll_offset;

    // Adjust scroll to keep selection visible
    if list_state.selected < scroll_offset {
        scroll_offset = list_state.selected;
    } else if list_state.selected >= scroll_offset + visible_height {
        scroll_offset = list_state.selected.saturating_sub(visible_height - 1);
    }

    // Wide panes have room for the modification time as well as the size.
    let show_modified = inner.width >= 56;

    let items: Vec<ListItem> = list_state
        .filtered_items
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_height)
        .map(|(display_idx, &item_idx)| {
            let item = &list_state.items[item_idx];
            let is_selected = display_idx == list_state.selected;
            let path = item.full_path();

            let mut line = Vec::new();
            if show_gutter {
                let (mark, style) = if state.selected_items.contains_key(&path) {
                    ("* ", Style::default().fg(theme.warning))
                } else if cut_paths.contains(&path) {
                    ("x ", Style::default().fg(theme.error))
                } else {
                    ("  ", Style::default())
                };
                line.push(Span::styled(mark, style));
            }
            line.push(Span::styled(
                format!("{} ", item.icon()),
                Style::default().fg(theme.dim),
            ));

            let name_style = match item.item_type {
                ItemType::Bucket => bucket_style(item.bucket.is_configured()),
                _ => item_type_style(&item.item_type),
            };
            line.push(Span::styled(item.name.as_str(), name_style));

            // Add metadata hints
            match item.item_type {
                ItemType::Object => {
                    if let Some(size) = item.size {
                        line.push(Span::styled(
                            format!("  {}", format_size(size)),
                            Style::default().fg(theme.dim),
                        ));
                    }
                    if show_modified && let Some(modified) = &item.last_modified {
                        line.push(Span::styled(
                            format!("  {}", format_datetime_short(modified)),
                            Style::default().fg(theme.dim),
                        ));
                    }
                    if let Some(class) = item
                        .storage_class
                        .as_deref()
                        .filter(|class| *class != "STANDARD")
                    {
                        line.push(Span::styled(
                            format!("  [{}]", class),
                            storage_class_style(class, theme),
                        ));
                    }
                }
                ItemType::Bucket => {
                    if let Some(profile) = &item.bucket.profile {
                        line.push(Span::styled(
                            format!("  [{}]", profile),
                            Style::default().fg(theme.dim),
                        ));
                    }
                }
                ItemType::Prefix | ItemType::LoadMore => {}
            }

            let style = if is_selected {
                // Use a visible but subtle selection background that preserves text colors
                Style::default()
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            ListItem::new(Line::from(line)).style(style)
        })
        .collect();

    let list = List::new(items).style(Style::default().bg(theme.surface));

    frame.render_widget(list, inner);
}
