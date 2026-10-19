use ratatui::style::{Color, Modifier, Style};

pub struct Theme {
    pub background: Color,
    pub surface: Color,
    pub panel: Color,
    pub selection: Color,
    pub primary: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub foreground: Color,
    pub dim: Color,
}

impl Theme {
    pub fn hopper_dark() -> Self {
        Self {
            background: Color::Rgb(15, 15, 20),    // #0f0f14
            surface: Color::Rgb(22, 22, 30),       // #16161e
            panel: Color::Rgb(26, 27, 38),         // #1a1b26
            selection: Color::Rgb(40, 45, 60),     // #282d3c - More visible selection
            primary: Color::Rgb(122, 162, 247),    // #7aa2f7
            success: Color::Rgb(158, 206, 106),    // #9ece6a
            warning: Color::Rgb(224, 175, 104),    // #e0af68
            error: Color::Rgb(247, 118, 142),      // #f7768e
            foreground: Color::Rgb(192, 202, 245), // #c0caf5
            dim: Color::Rgb(128, 128, 128),
        }
    }
}

pub fn item_type_style(item_type: &crate::client::ItemType) -> Style {
    use crate::client::ItemType;

    let color = match item_type {
        ItemType::Bucket => Color::Magenta,
        ItemType::Prefix => Color::Yellow,
        ItemType::Object => Color::White,
        ItemType::LoadMore => Color::DarkGray,
    };

    let mut style = Style::default().fg(color);
    if matches!(item_type, ItemType::Bucket | ItemType::Prefix) {
        style = style.add_modifier(Modifier::BOLD);
    }
    style
}

/// Discovered buckets are shown less prominently than configured ones.
pub fn bucket_style(configured: bool) -> Style {
    if configured {
        item_type_style(&crate::client::ItemType::Bucket)
    } else {
        Style::default().fg(Color::Rgb(190, 140, 220))
    }
}

pub fn storage_class_style(storage_class: &str, theme: &Theme) -> Style {
    match storage_class {
        "GLACIER" | "DEEP_ARCHIVE" => Style::default().fg(Color::Cyan),
        "STANDARD" => Style::default().fg(theme.dim),
        _ => Style::default().fg(theme.warning),
    }
}
