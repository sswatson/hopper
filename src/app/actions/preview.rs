use super::*;
use std::time::{Duration, Instant};

/// How much of an object to fetch for the preview pane.
pub(super) const PREVIEW_BYTES: u64 = 16 * 1024;

/// Delay before fetching a preview, so holding `j` doesn't fire a request
/// for every object scrolled past.
const PREVIEW_SETTLE: Duration = Duration::from_millis(120);

/// Storage classes whose bodies can't be read without a restore.
pub(super) fn is_archived(storage_class: Option<&str>) -> bool {
    matches!(storage_class, Some("GLACIER" | "DEEP_ARCHIVE"))
}

/// Records the object under the cursor as the next preview to fetch.
pub(super) fn note_selection(state: &mut AppState) {
    let Some(item) = state.selected_item().cloned() else {
        state.preview_candidate = None;
        return;
    };
    if item.item_type != ItemType::Object || state.previews.contains_key(&item.full_path()) {
        state.preview_candidate = None;
        return;
    }
    let unchanged = state
        .preview_candidate
        .as_ref()
        .is_some_and(|(candidate, _)| navigation::same_item(candidate, &item));
    if !unchanged {
        state.preview_candidate = Some((item, Instant::now()));
    }
}

pub(super) fn maybe_fetch_candidate_preview(state: &mut AppState) {
    let ready = state
        .preview_candidate
        .as_ref()
        .is_some_and(|(_, since)| since.elapsed() >= PREVIEW_SETTLE);
    if !ready {
        return;
    }
    let Some((item, _)) = state.preview_candidate.take() else {
        return;
    };
    let path = item.full_path();
    if state.previews.contains_key(&path) {
        return;
    }
    state.previews.insert(path.clone(), PreviewState::Loading);
    let client = state.client.clone();
    let tx = state.task_tx.clone();
    tokio::spawn(async move {
        let result = fetch_preview(&client, &item).await;
        let _ = tx.send(TaskResult::Preview { path, result });
    });
}

async fn fetch_preview(client: &S3Client, item: &S3Item) -> Result<ObjectPreview> {
    let details = client.head(&item.bucket, &item.key).await?;
    let size = details.content_length.or(item.size).unwrap_or(0);
    let storage_class = details
        .storage_class
        .clone()
        .or_else(|| item.storage_class.clone());

    let body = if size == 0 {
        PreviewBody::Empty
    } else if is_archived(storage_class.as_deref()) {
        PreviewBody::Skipped {
            reason: format!(
                "Stored in {}; restore it before reading.",
                storage_class.unwrap_or_default()
            ),
        }
    } else {
        match client
            .read_prefix_bytes(&item.bucket, &item.key, PREVIEW_BYTES)
            .await
        {
            Ok(bytes) => classify_body(&bytes, size),
            Err(err) => PreviewBody::Skipped {
                reason: format!("{:#}", err),
            },
        }
    };
    Ok(ObjectPreview { details, body })
}

/// Decides whether the first bytes of an object are displayable text.
pub(super) fn classify_body(bytes: &[u8], total_size: u64) -> PreviewBody {
    if bytes.is_empty() {
        return PreviewBody::Empty;
    }
    let magic: &[(&[u8], &str)] = &[
        (b"PAR1", "Parquet file"),
        (b"\x1f\x8b", "gzip-compressed data"),
        (b"PK\x03\x04", "zip archive"),
        (b"\x28\xb5\x2f\xfd", "zstd-compressed data"),
        (b"BZh", "bzip2-compressed data"),
        (b"\x89PNG", "PNG image"),
        (b"\xff\xd8\xff", "JPEG image"),
        (b"%PDF", "PDF document"),
        (b"ORC", "ORC file"),
        (b"Obj\x01", "Avro file"),
    ];
    for (prefix, description) in magic {
        if bytes.starts_with(prefix) {
            return PreviewBody::Binary {
                description: description.to_string(),
            };
        }
    }

    // A ranged read may cut a multi-byte character; drop the partial tail.
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) if err.error_len().is_none() => {
            std::str::from_utf8(&bytes[..err.valid_up_to()]).unwrap_or_default()
        }
        Err(_) => {
            return PreviewBody::Binary {
                description: "Binary data".to_string(),
            };
        }
    };
    if text.contains('\0') {
        return PreviewBody::Binary {
            description: "Binary data".to_string(),
        };
    }
    PreviewBody::Text {
        text: text.replace('\r', "").replace('\t', "    "),
        truncated: total_size > bytes.len() as u64,
    }
}

/// Starts summing object sizes under the selected prefix or bucket (or the
/// active location when the cursor is on an object).
pub(super) fn fetch_prefix_size(state: &mut AppState) {
    let target = match state.selected_item() {
        Some(item) if item.is_container() => {
            item.child_location().map(|loc| (item.full_path(), loc))
        }
        _ => state
            .active_location()
            .map(|location| (location.uri(), location)),
    };
    let Some((path, location)) = target else {
        state.set_status("Select a bucket or prefix to size".to_string());
        return;
    };
    if matches!(
        state.prefix_sizes.get(&path),
        Some(PrefixSizeState::Loading)
    ) {
        return;
    }
    state
        .prefix_sizes
        .insert(path.clone(), PrefixSizeState::Loading);
    state.set_status(format!("Computing size of {}...", path));
    let client = state.client.clone();
    let tx = state.task_tx.clone();
    tokio::spawn(async move {
        let result = client
            .list_recursive(&location.bucket, &location.prefix)
            .await
            .map(|objects| {
                let bytes = objects.iter().map(|object| object.size).sum();
                (objects.len() as u64, bytes)
            });
        let _ = tx.send(TaskResult::PrefixSize { path, result });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_detects_text_binary_and_formats() {
        assert!(matches!(
            classify_body(b"a,b\n1,2\n", 8),
            PreviewBody::Text {
                truncated: false,
                ..
            }
        ));
        assert!(matches!(
            classify_body(b"a,b\n", 100),
            PreviewBody::Text {
                truncated: true,
                ..
            }
        ));
        assert_eq!(
            classify_body(b"PAR1\x00\x00", 6),
            PreviewBody::Binary {
                description: "Parquet file".to_string()
            }
        );
        assert!(matches!(
            classify_body(b"abc\x00def", 7),
            PreviewBody::Binary { .. }
        ));
        assert!(matches!(
            classify_body(&[0xff, 0xfe, 0x41], 3),
            PreviewBody::Binary { .. }
        ));
    }

    #[test]
    fn classify_tolerates_a_multibyte_char_cut_by_the_range() {
        let text = "héllo".as_bytes();
        let cut = &text[..2]; // "h" plus the first byte of "é"
        match classify_body(cut, 100) {
            PreviewBody::Text { text, truncated } => {
                assert_eq!(text, "h");
                assert!(truncated);
            }
            other => panic!("expected text, got {:?}", other),
        }
    }
}
