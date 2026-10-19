use crate::app::{AppState, PrefixSizeState, PreviewState};
use crate::client::{BucketSource, ItemType, PreviewBody, S3Item};
use crate::ui::colors::{Theme, bucket_style, item_type_style, storage_class_style};
use crate::ui::helpers::{format_datetime, format_size};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub fn render_preview_pane(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.panel))
        .title(" Preview ")
        .style(Style::default().bg(theme.surface));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = match state.selected_item() {
        Some(item) => item_lines(item, state, theme),
        None => empty_level_lines(state, theme),
    };
    if lines.is_empty() {
        let empty = Paragraph::new("Select an item to see details")
            .style(Style::default().fg(theme.dim))
            .alignment(Alignment::Center);
        frame.render_widget(empty, inner);
        return;
    }

    let paragraph = Paragraph::new(lines)
        .style(Style::default().fg(theme.foreground))
        .wrap(Wrap { trim: false })
        .scroll((state.preview_scroll.min(u16::MAX as usize) as u16, 0));
    frame.render_widget(paragraph, inner);
}

fn field<'a>(label: &str, value: impl Into<String>, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{:<15}", format!("{}:", label)),
            Style::default().fg(theme.dim),
        ),
        Span::raw(value.into()),
    ])
}

fn styled_field<'a>(label: &str, value: Span<'a>, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{:<15}", format!("{}:", label)),
            Style::default().fg(theme.dim),
        ),
        value,
    ])
}

fn header_line<'a>(item: &S3Item) -> Line<'a> {
    let style = if item.item_type == ItemType::Bucket {
        bucket_style(item.bucket.is_configured())
    } else {
        item_type_style(&item.item_type)
    };
    Line::from(vec![
        Span::raw(item.icon()),
        Span::raw(" "),
        Span::styled(item.name.clone(), style.add_modifier(Modifier::BOLD)),
    ])
}

fn section<'a>(title: &str, theme: &Theme) -> Line<'a> {
    Line::from(Span::styled(
        title.to_string(),
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD),
    ))
}

fn size_lines<'a>(path: &str, state: &AppState, theme: &Theme) -> Line<'a> {
    match state.prefix_sizes.get(path) {
        None => styled_field(
            "Total size",
            Span::styled("press z to compute", Style::default().fg(theme.dim)),
            theme,
        ),
        Some(PrefixSizeState::Loading) => styled_field(
            "Total size",
            Span::styled("computing...", Style::default().fg(theme.warning)),
            theme,
        ),
        Some(PrefixSizeState::Loaded { objects, bytes }) => field(
            "Total size",
            format!(
                "{} in {} object{}",
                format_size(*bytes),
                objects,
                if *objects == 1 { "" } else { "s" }
            ),
            theme,
        ),
        Some(PrefixSizeState::Failed(err)) => styled_field(
            "Total size",
            Span::styled(err.clone(), Style::default().fg(theme.error)),
            theme,
        ),
    }
}

fn item_lines<'a>(item: &S3Item, state: &AppState, theme: &Theme) -> Vec<Line<'a>> {
    let mut lines = vec![header_line(item), Line::from("")];
    match item.item_type {
        ItemType::Bucket => bucket_lines(&mut lines, item, state, theme),
        ItemType::Prefix => {
            lines.push(field("URI", item.full_path(), theme));
            lines.push(size_lines(&item.full_path(), state, theme));
        }
        ItemType::Object => object_lines(&mut lines, item, state, theme),
        ItemType::LoadMore => {
            lines.push(Line::from(Span::styled(
                "The listing has more entries. Press Enter to load the next page.",
                Style::default().fg(theme.dim),
            )));
        }
    }
    lines
}

fn bucket_lines<'a>(lines: &mut Vec<Line<'a>>, item: &S3Item, state: &AppState, theme: &Theme) {
    let bucket = &item.bucket;
    lines.push(field("Bucket", format!("s3://{}/", bucket.name), theme));
    if !bucket.prefix.is_empty() {
        lines.push(field("Opens at", bucket.prefix.clone(), theme));
    }
    let profile = state
        .client
        .effective_profile(bucket)
        .unwrap_or_else(|| "default credential chain".to_string());
    lines.push(field("Profile", profile, theme));
    let region = state
        .client
        .known_region(bucket)
        .unwrap_or_else(|| "detected on first use".to_string());
    lines.push(field("Region", region, theme));
    let source = match &bucket.source {
        BucketSource::Config(path) => format!("config ({})", path.display()),
        BucketSource::CommandLine => "command line (-b)".to_string(),
        BucketSource::Discovered => "ListBuckets".to_string(),
    };
    lines.push(field("Listed from", source, theme));
    lines.push(size_lines(&item.full_path(), state, theme));
    if !bucket.is_configured() && bucket.source == BucketSource::Discovered {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Discovered buckets come and go with ListBuckets. Add it to your config to pin it.",
            Style::default().fg(theme.dim),
        )));
    }
}

fn object_lines<'a>(lines: &mut Vec<Line<'a>>, item: &S3Item, state: &AppState, theme: &Theme) {
    let preview = state.previews.get(&item.full_path());
    let details = match preview {
        Some(PreviewState::Loaded(preview)) => Some(&preview.details),
        _ => None,
    };

    lines.push(field("URI", item.full_path(), theme));
    if let Some(size) = details.and_then(|d| d.content_length).or(item.size) {
        lines.push(field(
            "Size",
            format!("{} ({} bytes)", format_size(size), size),
            theme,
        ));
    }
    if let Some(modified) = details.and_then(|d| d.last_modified).or(item.last_modified) {
        lines.push(field("Modified", format_datetime(&modified), theme));
    }
    if let Some(class) = details
        .and_then(|d| d.storage_class.clone())
        .or_else(|| item.storage_class.clone())
    {
        let style = storage_class_style(&class, theme);
        lines.push(styled_field(
            "Storage class",
            Span::styled(class, style),
            theme,
        ));
    }
    if let Some(details) = details {
        if let Some(content_type) = &details.content_type {
            lines.push(field("Content type", content_type.clone(), theme));
        }
        if let Some(encoding) = &details.content_encoding {
            lines.push(field("Encoding", encoding.clone(), theme));
        }
        if let Some(cache) = &details.cache_control {
            lines.push(field("Cache control", cache.clone(), theme));
        }
        if let Some(etag) = &details.etag {
            lines.push(field("ETag", etag.trim_matches('"').to_string(), theme));
        }
        if let Some(sse) = &details.server_side_encryption {
            lines.push(field("Encryption", sse.clone(), theme));
        }
        if let Some(version) = &details.version_id {
            lines.push(field("Version", version.clone(), theme));
        }
        if !details.metadata.is_empty() {
            lines.push(Line::from(""));
            lines.push(section("Metadata", theme));
            for (key, value) in &details.metadata {
                lines.push(field(key, value.clone(), theme));
            }
        }
    }

    lines.push(Line::from(""));
    match preview {
        None | Some(PreviewState::Loading) => lines.push(Line::from(Span::styled(
            "Loading preview...",
            Style::default().fg(theme.dim),
        ))),
        Some(PreviewState::Failed(err)) => lines.push(Line::from(Span::styled(
            err.clone(),
            Style::default().fg(theme.error),
        ))),
        Some(PreviewState::Loaded(preview)) => match &preview.body {
            PreviewBody::Text { text, truncated } => {
                lines.push(section("Contents", theme));
                for line in text.lines() {
                    lines.push(Line::from(Span::raw(line.to_string())));
                }
                if *truncated {
                    lines.push(Line::from(Span::styled(
                        "... (preview truncated; Enter opens the full object in $EDITOR)",
                        Style::default().fg(theme.dim),
                    )));
                }
            }
            PreviewBody::Binary { description } => lines.push(Line::from(Span::styled(
                format!("{} (no text preview)", description),
                Style::default().fg(theme.dim),
            ))),
            PreviewBody::Empty => lines.push(Line::from(Span::styled(
                "(empty object)",
                Style::default().fg(theme.dim),
            ))),
            PreviewBody::Skipped { reason } => lines.push(Line::from(Span::styled(
                reason.clone(),
                Style::default().fg(theme.warning),
            ))),
        },
    }
}

/// When the active pane is empty, describe the location instead.
fn empty_level_lines<'a>(state: &AppState, theme: &Theme) -> Vec<Line<'a>> {
    let Some(location) = state.active_location() else {
        return Vec::new();
    };
    vec![
        field("URI", location.uri(), theme),
        Line::from(""),
        Line::from(Span::styled(
            "No objects here. Press U to upload or a to create one.",
            Style::default().fg(theme.dim),
        )),
    ]
}
