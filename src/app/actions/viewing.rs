use super::editing::edit_temp_path;
use super::terminal::run_blocking_with_terminal_suspended;
use super::*;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;

/// `v`: views the selected object, or says why it can't.
pub(super) fn view_selected(state: &mut AppState) {
    match state.selected_item().cloned() {
        Some(item) if item.item_type == ItemType::Object => begin_view_object(state, item),
        _ => state.set_status("Select an object to view".to_string()),
    }
}

/// Downloads the object in the background; `run_viewer` takes over once the
/// bytes are on disk.
fn begin_view_object(state: &mut AppState, item: S3Item) {
    let Some(viewer) = state.config.viewer_for(&item.key) else {
        let extension = Path::new(&item.key)
            .extension()
            .map(|extension| format!(".{}", extension.to_string_lossy()))
            .unwrap_or_else(|| "files without an extension".to_string());
        state.set_status(format!(
            "No viewer for {}; add a [[viewers]] entry to your config",
            extension
        ));
        return;
    };
    let command = viewer.command.clone();
    if preview::is_archived(item.storage_class.as_deref()) {
        state.set_status(format!(
            "{} is in {}; restore it before viewing",
            item.full_path(),
            item.storage_class.clone().unwrap_or_default()
        ));
        return;
    }

    let path = edit_temp_path(&item.bucket.name, &item.key);
    state.running_operations += 1;
    state.operation_progress = Some(format!("Downloading {}...", item.full_path()));
    let client = state.client.clone();
    let tx = state.task_tx.clone();
    tokio::spawn(async move {
        let result = async {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            client.download_to(&item.bucket, &item.key, &path).await?;
            Ok(ViewRequest {
                bucket: item.bucket.clone(),
                key: item.key.clone(),
                path,
                command,
            })
        }
        .await;
        let _ = tx.send(TaskResult::ViewReady(result));
    });
}

pub(super) async fn run_viewer(state: &mut AppState, request: ViewRequest) -> Result<()> {
    state.running_operations = state.running_operations.saturating_sub(1);
    if state.running_operations == 0 {
        state.operation_progress = None;
    }
    let uri = format!("s3://{}/{}", request.bucket.name, request.key);

    let (program, args) = request
        .command
        .split_first()
        .ok_or_else(|| anyhow!("viewer command is empty"))?;
    let program = program.clone();
    let args = args.to_vec();
    let file_arg = request.path.clone();
    let status =
        run_blocking_with_terminal_suspended(move || run_foreground(&program, &args, &file_arg))
            .await?;
    state.needs_redraw = true;

    match status {
        // Ctrl+C is the normal way to stop a viewer, so it isn't an error.
        Ok(status) if status.success() || status.signal() == Some(libc::SIGINT) => {
            state.set_status(format!("Closed {}", uri))
        }
        Ok(status) => state.set_status(format!("Viewer exited with {} ({})", status, uri)),
        Err(err) => state.set_status(format!("Failed to launch {}: {}", request.command[0], err)),
    }
    Ok(())
}

/// Runs the viewer in the foreground and waits for it. Ctrl+C is delivered to
/// the whole terminal process group, so hopper ignores SIGINT while it waits
/// and only the viewer stops. The ignore is set after spawning so the child
/// keeps the default SIGINT disposition.
fn run_foreground(program: &str, args: &[String], file: &Path) -> std::io::Result<ExitStatus> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .arg(file)
        .spawn()?;
    // SAFETY: plain signal(2) calls with constant arguments; the previous
    // disposition is restored before returning.
    let previous = unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
    let status = child.wait();
    unsafe { libc::signal(libc::SIGINT, previous) };
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_run_passes_path_last_and_reports_exit_status() {
        let status = run_foreground("true", &["--flag".to_string()], Path::new("/tmp/x")).unwrap();
        assert!(status.success());

        let status = run_foreground("false", &[], Path::new("x")).unwrap();
        assert!(!status.success());
    }

    #[test]
    fn missing_viewer_program_is_a_spawn_error() {
        let err = run_foreground("hopper-no-such-viewer", &[], Path::new("x")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
