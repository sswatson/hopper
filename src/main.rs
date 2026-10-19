use anyhow::{Result, anyhow};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use hopper::app::config::{Config, append_bucket_to_file, load_config, writable_config_path};
use hopper::app::{AppMode, actions, events::AppEvent, state::AppState};
use hopper::client::{BucketEntry, BucketSource, S3Client, parse_bucket_spec};
use hopper::ui;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "hopper")]
#[command(version)]
#[command(about = "A fast TUI for browsing S3 buckets", long_about = None)]
struct Args {
    /// Run with mock data (no AWS connection required)
    #[arg(long)]
    demo: bool,

    /// Default AWS profile (overrides `profile` in the config file)
    #[arg(long)]
    profile: Option<String>,

    /// Region to use before a bucket's own region is known
    #[arg(long)]
    region: Option<String>,

    /// Extra bucket to show, as `bucket`, `s3://bucket/prefix`, or
    /// `bucket@profile`. Repeatable.
    #[arg(short = 'b', long = "bucket", value_name = "BUCKET")]
    buckets: Vec<String>,

    /// Use this config file instead of ~/.config/hopper/config.toml (and the
    /// project's .config/hopper/config.toml)
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Don't call ListBuckets; show only configured buckets
    #[arg(long)]
    no_discover: bool,

    /// Print the config file path that `a` writes to, and exit
    #[arg(long)]
    print_config_path: bool,
}

/// Parses `-b` values: `bucket`, `s3://bucket/prefix`, `bucket@profile`.
fn parse_cli_bucket(spec: &str) -> Result<BucketEntry> {
    let (location, profile) = match spec.rsplit_once('@') {
        Some((location, profile)) if !profile.is_empty() => (location, Some(profile.to_string())),
        _ => (spec, None),
    };
    let (name, prefix) =
        parse_bucket_spec(location).ok_or_else(|| anyhow!("invalid bucket {:?}", spec))?;
    Ok(BucketEntry::new(name)
        .with_profile(profile)
        .with_prefix(prefix)
        .with_source(BucketSource::CommandLine))
}

/// Writes a sample config for `--demo` so adding and removing buckets
/// behaves exactly like the real thing, without touching the user's config.
fn demo_config() -> Result<(Config, PathBuf)> {
    let path = std::env::temp_dir()
        .join(format!("hopper-demo-{}", std::process::id()))
        .join("config.toml");
    let _ = std::fs::remove_file(&path);
    for entry in [
        BucketEntry {
            label: Some("Partner feed (cross-account)".to_string()),
            ..BucketEntry::new("partner-data-feed").with_profile(Some("partner".to_string()))
        },
        BucketEntry::new("acme-archive").with_prefix("2023/"),
    ] {
        append_bucket_to_file(&path, &entry)?;
    }
    Ok((load_config(Some(&path))?, path))
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
}

// Restores the terminal on every exit path (normal return, error, panic
// unwind) so a crash never strands the user in raw mode on the alternate
// screen.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Leave the alternate screen before the default hook prints, so the
        // panic message is actually visible.
        restore_terminal();
        default_hook(info);
    }));
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if args.print_config_path {
        match writable_config_path(args.config.as_deref()) {
            Some(path) => println!("{}", path.display()),
            None => return Err(anyhow!("no config directory available on this system")),
        }
        return Ok(());
    }

    let (mut config, write_path) = if args.demo {
        let (config, path) = demo_config()?;
        (config, Some(path))
    } else {
        match load_config(args.config.as_deref()) {
            Ok(config) => (config, writable_config_path(args.config.as_deref())),
            Err(e) => {
                eprintln!("Error: {:#}\n", e);
                std::process::exit(1);
            }
        }
    };
    if args.profile.is_some() {
        config.default_profile = args.profile.clone();
    }
    if args.region.is_some() {
        config.default_region = args.region.clone();
    }
    if args.no_discover {
        config.discover.clear();
    }
    let mut cli_buckets = Vec::new();
    for spec in &args.buckets {
        cli_buckets.push(parse_cli_bucket(spec)?);
    }

    let client = if args.demo {
        S3Client::mock()
    } else {
        S3Client::new(
            config.default_profile.clone(),
            config.default_region.clone(),
        )
    };

    // Setup terminal
    install_panic_hook();
    enable_raw_mode()?;
    let _terminal_guard = TerminalGuard;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state
    let mut state = AppState::new(client, config);
    state.config_write_path = write_path;
    state.cli_buckets = cli_buckets;
    actions::initialize_app(&mut state);

    // Run the app; _terminal_guard restores the terminal on the way out.
    let result = run_app(&mut terminal, &mut state).await;
    actions::cleanup_temp_files();
    result
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
) -> Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    loop {
        actions::poll_background_tasks(state).await?;
        if state.is_busy() {
            state.spinner_index = state.spinner_index.wrapping_add(1);
        }

        if state.needs_redraw {
            terminal.clear()?;
            state.needs_redraw = false;
        }

        // Render UI
        terminal.draw(|f| ui::render(f, state))?;

        // Check if we should quit
        if state.should_quit {
            break;
        }

        // Handle events with timeout
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            // Only process key press events, not release
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Global quit hotkey. Keep Ctrl+C global, but let `q` be typed in all text-input modes.
            if should_trigger_global_quit(key.code, key.modifiers, state.mode) {
                actions::handle_event(state, AppEvent::Quit).await?;
                continue;
            }

            // All other key handling (including text-input modes) lives in
            // AppEvent::from_key_event; dispatch routes the event by mode.
            if let Some(app_event) = AppEvent::from_key_event(key, state.mode) {
                actions::handle_event(state, app_event).await?;
            }
        }
    }

    Ok(())
}

fn should_trigger_global_quit(code: KeyCode, modifiers: KeyModifiers, mode: AppMode) -> bool {
    matches!(
        (code, modifiers),
        (KeyCode::Char('c'), KeyModifiers::CONTROL)
    ) || (!mode.is_text_input()
        && matches!((code, modifiers), (KeyCode::Char('q'), KeyModifiers::NONE)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_quit_hotkey_is_disabled_for_q_in_text_input_modes() {
        assert!(!should_trigger_global_quit(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            AppMode::Input
        ));
        assert!(!should_trigger_global_quit(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            AppMode::BucketForm
        ));
    }

    #[test]
    fn global_quit_hotkey_still_works_in_normal_mode() {
        assert!(should_trigger_global_quit(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            AppMode::Normal
        ));
    }

    #[test]
    fn ctrl_c_stays_global_even_in_text_input_mode() {
        assert!(should_trigger_global_quit(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            AppMode::Input
        ));
    }

    #[test]
    fn cli_bucket_spec_supports_prefix_and_profile() {
        let entry = parse_cli_bucket("s3://feed/exports/2024@legacy").unwrap();
        assert_eq!(entry.name, "feed");
        assert_eq!(entry.prefix, "exports/2024/");
        assert_eq!(entry.profile.as_deref(), Some("legacy"));
        let plain = parse_cli_bucket("plain").unwrap();
        assert_eq!(plain.profile, None);
    }

    #[test]
    fn cli_supports_version_flag() {
        let error = Args::try_parse_from(["hopper", "--version"])
            .expect_err("--version should stop parsing with version output");

        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
        assert!(error.to_string().contains(env!("CARGO_PKG_VERSION")));
    }
}
