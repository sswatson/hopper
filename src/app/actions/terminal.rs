use anyhow::Result;

/// Leaves the alternate screen and raw mode while `run` executes (e.g. an
/// editor), then restores both. The caller should force a full redraw.
pub(super) async fn run_blocking_with_terminal_suspended<T, F>(run: F) -> Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let result = tokio::task::spawn_blocking(move || {
        use crossterm::{
            execute,
            terminal::{
                EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
            },
        };
        use std::io::{self, Write};

        let mut stdout = io::stdout();
        let _ = disable_raw_mode();
        let _ = execute!(stdout, LeaveAlternateScreen);
        let _ = stdout.flush();

        let result = run();

        let _ = execute!(stdout, EnterAlternateScreen);
        let _ = enable_raw_mode();
        let _ = stdout.flush();

        result
    })
    .await?;
    Ok(result)
}
