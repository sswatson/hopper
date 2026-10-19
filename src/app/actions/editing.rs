use super::terminal::run_blocking_with_terminal_suspended;
use super::*;
use std::path::PathBuf;

/// Objects larger than this are not opened in the editor (use `D` instead).
const MAX_EDIT_BYTES: u64 = 50 * 1024 * 1024;

fn editor_command() -> Option<(String, Vec<String>)> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::env::var("EDITOR").ok())
        .filter(|value| !value.trim().is_empty())?;
    let mut parts = editor.split_whitespace().map(str::to_string);
    let command = parts.next()?;
    Some((command, parts.collect()))
}

fn session_temp_dir() -> PathBuf {
    std::env::temp_dir().join(format!("hopper-{}", std::process::id()))
}

/// Removes this session's editor scratch files.
pub fn cleanup_temp_files() {
    let _ = std::fs::remove_dir_all(session_temp_dir());
}

/// Local scratch path mirroring the key, so the editor sees the real file
/// name (and picks the right syntax highlighting). `..` and empty segments
/// are dropped so a key can never point outside the scratch directory.
pub(super) fn edit_temp_path(bucket: &str, key: &str) -> PathBuf {
    let mut path = session_temp_dir().join(sanitize_segment(bucket));
    let segments: Vec<&str> = key
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != "." && *segment != "..")
        .collect();
    if segments.is_empty() {
        return path.join("object");
    }
    for segment in segments {
        path.push(sanitize_segment(segment));
    }
    path
}

fn sanitize_segment(segment: &str) -> String {
    segment.replace('\\', "_")
}

/// Downloads the object in the background; `run_editor` takes over once the
/// bytes are on disk.
pub(super) fn begin_edit_object(state: &mut AppState, item: S3Item) {
    if editor_command().is_none() {
        state.set_status("EDITOR is not set".to_string());
        return;
    }
    if item.size.unwrap_or(0) > MAX_EDIT_BYTES {
        state.set_status(format!(
            "{} is too large to open in the editor; press D to download it",
            item.full_path()
        ));
        return;
    }
    if preview::is_archived(item.storage_class.as_deref()) {
        state.set_status(format!(
            "{} is in {}; restore it before opening",
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
            let bytes = client.read_all(&item.bucket, &item.key).await?;
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, &bytes).await?;
            Ok(EditRequest {
                bucket: item.bucket.clone(),
                key: item.key.clone(),
                path,
                original: Some(bytes),
            })
        }
        .await;
        let _ = tx.send(TaskResult::EditReady(result));
    });
}

/// Opens the editor on a not-yet-existing file; the object is created only
/// if the user saves.
pub(super) fn begin_create_object(
    state: &mut AppState,
    location: &Location,
    key: String,
) -> Result<()> {
    if editor_command().is_none() {
        return Err(anyhow!("EDITOR is not set"));
    }
    let path = edit_temp_path(&location.bucket.name, &key);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(&path);
    state.running_operations += 1;
    let _ = state.task_tx.send(TaskResult::EditReady(Ok(EditRequest {
        bucket: location.bucket.clone(),
        key,
        path,
        original: None,
    })));
    Ok(())
}

pub(super) async fn run_editor(state: &mut AppState, request: EditRequest) -> Result<()> {
    state.running_operations = state.running_operations.saturating_sub(1);
    if state.running_operations == 0 {
        state.operation_progress = None;
    }
    let uri = format!("s3://{}/{}", request.bucket.name, request.key);

    let Some((command, args)) = editor_command() else {
        state.set_status("EDITOR is not set".to_string());
        return Ok(());
    };
    let file_arg = request.path.clone();
    let status = run_blocking_with_terminal_suspended(move || {
        std::process::Command::new(&command)
            .args(&args)
            .arg(&file_arg)
            .status()
    })
    .await?;
    state.needs_redraw = true;

    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            state.set_status(format!(
                "Editor exited with {}; {} not uploaded",
                status, uri
            ));
            return Ok(());
        }
        Err(err) => {
            state.set_status(format!("Failed to launch editor: {}", err));
            return Ok(());
        }
    }

    if !request.path.exists() {
        state.set_status(format!("Nothing saved; {} not created", uri));
        return Ok(());
    }
    let edited = std::fs::read(&request.path)?;
    if request.original.as_deref() == Some(edited.as_slice()) {
        state.set_status(format!("No changes to {}", uri));
        return Ok(());
    }

    let created = request.original.is_none();
    let client = state.client.clone();
    let parent = Location::new(
        request.bucket.clone(),
        navigation::parent_prefix(&request.key),
    );
    operations::spawn_operation(state, format!("Uploading {}...", uri), async move {
        let result = client
            .put_bytes(&request.bucket, &request.key, edited)
            .await
            .map(|_| {
                if created {
                    format!("Created {}", uri)
                } else {
                    format!("Uploaded changes to {}", uri)
                }
            });
        (result, vec![parent])
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_temp_path_mirrors_key_and_cannot_escape() {
        let base = session_temp_dir();
        assert_eq!(
            edit_temp_path("b", "data/report.csv"),
            base.join("b").join("data").join("report.csv")
        );
        assert_eq!(
            edit_temp_path("b", "../../etc/passwd"),
            base.join("b").join("etc").join("passwd")
        );
        assert_eq!(edit_temp_path("b", "/"), base.join("b").join("object"));
    }
}
