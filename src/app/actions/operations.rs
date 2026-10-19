use super::navigation::{key_basename, parent_prefix, refresh_root_level, same_item};
use super::*;
use crate::app::config::{append_bucket_to_file, remove_bucket_from_file};
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use tokio::sync::mpsc::UnboundedSender;

/// Parallel requests per transfer.
const TRANSFER_CONCURRENCY: usize = 8;

/// Keys to act on, grouped by bucket (and profile, which picks credentials).
type KeysByBucket = BTreeMap<(String, Option<String>), (Arc<BucketEntry>, Vec<String>)>;

pub(super) fn counted_noun(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {}", singular)
    } else {
        format!("{} {}", count, plural)
    }
}

/// Runs a long operation in the background. It reports progress through the
/// status bar and, when done, a summary plus the locations to refresh.
pub(super) fn spawn_operation<F>(state: &mut AppState, start_message: String, operation: F)
where
    F: Future<Output = (Result<String>, Vec<Location>)> + Send + 'static,
{
    state.running_operations += 1;
    state.operation_progress = Some(start_message);
    let tx = state.task_tx.clone();
    tokio::spawn(async move {
        let (result, refresh) = operation.await;
        let _ = tx.send(TaskResult::OperationDone { result, refresh });
    });
}

#[derive(Clone)]
pub(super) struct Progress(UnboundedSender<TaskResult>);

impl Progress {
    fn report(&self, message: String) {
        let _ = self.0.send(TaskResult::Progress(message));
    }
}

#[derive(Debug, Default)]
pub(super) struct Outcome {
    pub done: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

impl Outcome {
    /// `"Downloaded 3 objects to /tmp, 1 skipped (already exists locally)"`,
    /// as an error if anything failed.
    fn finish(self, summary: String, skip_reason: &str) -> Result<String> {
        let mut message = summary;
        if self.skipped > 0 {
            message.push_str(&format!(", {} skipped ({})", self.skipped, skip_reason));
        }
        match self.errors.first() {
            None => Ok(message),
            Some(first) => Err(anyhow!(
                "{}, {} failed: {}",
                message,
                self.errors.len(),
                first
            )),
        }
    }
}

/// Runs `work` over `jobs` with bounded parallelism, reporting `verb n/total`.
async fn run_bounded<T, F, Fut>(jobs: Vec<T>, progress: &Progress, verb: &str, work: F) -> Outcome
where
    T: Send + 'static,
    F: Fn(T) -> Fut,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    let total = jobs.len();
    let mut outcome = Outcome::default();
    let mut pending = jobs.into_iter();
    let mut set = tokio::task::JoinSet::new();
    let mut finished = 0usize;
    loop {
        while set.len() < TRANSFER_CONCURRENCY {
            match pending.next() {
                Some(job) => {
                    set.spawn(work(job));
                }
                None => break,
            }
        }
        let Some(joined) = set.join_next().await else {
            break;
        };
        finished += 1;
        match joined {
            Ok(Ok(())) => outcome.done += 1,
            Ok(Err(err)) => outcome.errors.push(format!("{:#}", err)),
            Err(err) => outcome.errors.push(err.to_string()),
        }
        progress.report(format!("{} {}/{}", verb, finished, total));
    }
    outcome
}

/// Lists the objects an item stands for: itself for an object, everything
/// underneath for a prefix or bucket.
async fn expand_item(client: &S3Client, item: &S3Item) -> Result<Vec<ObjectSummary>> {
    match item.item_type {
        ItemType::Object => Ok(vec![ObjectSummary {
            key: item.key.clone(),
            size: item.size.unwrap_or(0),
            last_modified: item.last_modified,
            storage_class: item.storage_class.clone(),
            etag: item.etag.clone(),
        }]),
        ItemType::Prefix | ItemType::Bucket => client.list_recursive(&item.bucket, &item.key).await,
        ItemType::LoadMore => Ok(Vec::new()),
    }
}

/// Converts a key-relative path into a local relative path, refusing
/// anything that could escape the destination (`..`, absolute paths).
pub(super) fn safe_relative_path(relative: &str) -> Option<PathBuf> {
    let path = Path::new(relative);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!clean.as_os_str().is_empty()).then_some(clean)
}

/// Where each object of `item` lands under `destination`: an object goes to
/// its file name; a prefix or bucket recreates its folder and everything
/// below it. Folder-marker keys (ending in `/`) become directories.
pub(super) fn plan_download(
    item: &S3Item,
    objects: &[ObjectSummary],
    destination: &Path,
) -> (Vec<(String, PathBuf)>, Vec<PathBuf>, Vec<String>) {
    let root_name = match item.item_type {
        ItemType::Bucket => format!("{}/", item.bucket.name),
        _ => key_basename(&item.key),
    };
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut rejected = Vec::new();
    for object in objects {
        let relative = match item.item_type {
            ItemType::Object => root_name.clone(),
            _ => format!(
                "{}{}",
                root_name,
                object
                    .key
                    .strip_prefix(item.key.as_str())
                    .unwrap_or(&object.key)
            ),
        };
        match safe_relative_path(&relative) {
            Some(path) if object.key.ends_with('/') => directories.push(destination.join(path)),
            Some(path) => files.push((object.key.clone(), destination.join(path))),
            None => rejected.push(object.key.clone()),
        }
    }
    (files, directories, rejected)
}

pub(super) fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

fn current_dir_input() -> String {
    std::env::current_dir()
        .map(|dir| {
            let mut text = dir.display().to_string();
            if !text.ends_with('/') {
                text.push('/');
            }
            text
        })
        .unwrap_or_else(|_| "./".to_string())
}

fn open_input(state: &mut AppState, purpose: InputPurpose, buffer: String) {
    state.text_input = Some(TextInput {
        purpose,
        buffer,
        hint: None,
    });
    state.mode = AppMode::Input;
}

pub(super) fn start_download(state: &mut AppState) {
    let items = state.target_items();
    if items.is_empty() {
        state.set_status("Nothing to download".to_string());
        return;
    }
    open_input(state, InputPurpose::Download { items }, current_dir_input());
}

fn execute_download(state: &mut AppState, items: Vec<S3Item>, destination_text: &str) {
    let destination = expand_tilde(destination_text.trim());
    let client = state.client.clone();
    let progress = Progress(state.task_tx.clone());
    let label = destination.display().to_string();
    state.selected_items.clear();
    spawn_operation(state, "Preparing download...".to_string(), async move {
        let result = async {
            let mut jobs = Vec::new();
            let mut skipped = 0usize;
            let mut rejected = Vec::new();
            for item in &items {
                progress.report(format!("Listing {}...", item.full_path()));
                let objects = expand_item(&client, item).await?;
                let (files, directories, bad) = plan_download(item, &objects, &destination);
                for directory in directories {
                    tokio::fs::create_dir_all(&directory).await?;
                }
                rejected.extend(bad);
                for (key, path) in files {
                    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
                        skipped += 1;
                    } else {
                        jobs.push((item.bucket.clone(), key, path));
                    }
                }
            }
            let mut outcome = run_bounded(jobs, &progress, "Downloading", |(bucket, key, path)| {
                let client = client.clone();
                async move { client.download_to(&bucket, &key, &path).await }
            })
            .await;
            outcome.skipped += skipped;
            outcome.errors.extend(
                rejected
                    .into_iter()
                    .map(|key| format!("unsafe local path for key {:?}", key)),
            );
            let summary = format!(
                "Downloaded {} to {}",
                counted_noun(outcome.done, "object", "objects"),
                label
            );
            outcome.finish(summary, "already exists locally")
        }
        .await;
        (result, Vec::new())
    });
}

pub(super) fn start_upload(state: &mut AppState) {
    let Some(target) = state.active_location() else {
        state.set_status("Open a bucket first, then press U to upload into it".to_string());
        return;
    };
    open_input(state, InputPurpose::Upload { target }, current_dir_input());
}

/// Files under `root` (recursively for directories) with their key suffixes.
fn collect_upload_files(root: &Path) -> Result<Vec<(PathBuf, String)>> {
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| anyhow!("{} has no file name", root.display()))?;
    let metadata = std::fs::metadata(root)?;
    if metadata.is_file() {
        return Ok(vec![(root.to_path_buf(), name)]);
    }
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), format!("{}/", name))];
    while let Some((dir, key_prefix)) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?.collect::<std::io::Result<_>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let file_name = entry.file_name().to_string_lossy().to_string();
            // Don't follow directory symlinks (they can loop).
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push((entry.path(), format!("{}{}/", key_prefix, file_name)));
            } else if std::fs::metadata(entry.path())
                .map(|m| m.is_file())
                .unwrap_or(false)
            {
                files.push((entry.path(), format!("{}{}", key_prefix, file_name)));
            }
        }
    }
    Ok(files)
}

fn execute_upload(state: &mut AppState, target: Location, source_text: &str) {
    let source = expand_tilde(source_text.trim().trim_end_matches('/'));
    if !source.exists() {
        state.set_status(format!("{} does not exist", source.display()));
        return;
    }
    let client = state.client.clone();
    let progress = Progress(state.task_tx.clone());
    let refresh = vec![target.clone()];
    spawn_operation(
        state,
        format!("Uploading {}...", source.display()),
        async move {
            let result = async {
                let files = tokio::task::spawn_blocking({
                    let source = source.clone();
                    move || collect_upload_files(&source)
                })
                .await??;
                let base_key = format!(
                    "{}{}",
                    target.prefix,
                    source
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default()
                );
                let existing: HashSet<String> = client
                    .list_recursive(&target.bucket, &base_key)
                    .await?
                    .into_iter()
                    .map(|object| object.key)
                    .collect();
                let mut skipped = 0usize;
                let mut jobs = Vec::new();
                for (path, suffix) in files {
                    let key = format!("{}{}", target.prefix, suffix);
                    if existing.contains(&key) {
                        skipped += 1;
                    } else {
                        jobs.push((target.bucket.clone(), key, path));
                    }
                }
                let mut outcome =
                    run_bounded(jobs, &progress, "Uploading", |(bucket, key, path)| {
                        let client = client.clone();
                        async move { client.upload_file(&bucket, &key, &path).await }
                    })
                    .await;
                outcome.skipped += skipped;
                let summary = format!(
                    "Uploaded {} to {}",
                    counted_noun(outcome.done, "file", "files"),
                    target.uri()
                );
                outcome.finish(summary, "already exists in S3")
            }
            .await;
            (result, refresh)
        },
    );
}

/// Completes the last path segment of `input` against the local filesystem.
/// Returns the new buffer and a hint listing ambiguous candidates.
pub(super) fn complete_local_path(input: &str) -> (String, Option<String>) {
    let (dir_part, file_prefix) = match input.rfind('/') {
        Some(slash) => (&input[..=slash], &input[slash + 1..]),
        None => ("", input),
    };
    let dir = if dir_part.is_empty() {
        PathBuf::from(".")
    } else {
        expand_tilde(dir_part)
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (
            input.to_string(),
            Some(format!("Can't read {}", dir.display())),
        );
    };
    let mut matches: Vec<(String, bool)> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(file_prefix)
                || (name.starts_with('.') && !file_prefix.starts_with('.'))
            {
                return None;
            }
            let is_dir = entry.path().is_dir();
            Some((name, is_dir))
        })
        .collect();
    matches.sort();

    match matches.len() {
        0 => (input.to_string(), Some("No matches".to_string())),
        1 => {
            let (name, is_dir) = &matches[0];
            let suffix = if *is_dir { "/" } else { "" };
            (format!("{}{}{}", dir_part, name, suffix), None)
        }
        _ => {
            let mut common = matches[0].0.clone();
            for (name, _) in &matches[1..] {
                while !name.starts_with(&common) {
                    common.pop();
                }
            }
            let names: Vec<String> = matches
                .iter()
                .take(12)
                .map(|(name, is_dir)| {
                    if *is_dir {
                        format!("{}/", name)
                    } else {
                        name.clone()
                    }
                })
                .collect();
            let more = if matches.len() > 12 {
                format!("  (+{} more)", matches.len() - 12)
            } else {
                String::new()
            };
            (
                format!("{}{}", dir_part, common),
                Some(format!("{}{}", names.join("  "), more)),
            )
        }
    }
}

pub(super) fn start_create(state: &mut AppState) {
    match state.active_location() {
        None => {
            state.bucket_form = BucketForm::default();
            state.mode = AppMode::BucketForm;
        }
        Some(target) => open_input(state, InputPurpose::Create { target }, String::new()),
    }
}

fn execute_create(state: &mut AppState, target: Location, name: &str) -> Result<()> {
    let name = name.trim().trim_start_matches('/');
    if name.is_empty() {
        return Err(anyhow!("Name is empty"));
    }
    let key = format!("{}{}", target.prefix, name);
    if name.ends_with('/') {
        // S3 has no directories; a zero-byte object named `prefix/` makes an
        // empty one show up in listings.
        let client = state.client.clone();
        let uri = format!("s3://{}/{}", target.bucket.name, key);
        let refresh = vec![target.clone()];
        spawn_operation(state, format!("Creating {}...", uri), async move {
            let result = client
                .put_bytes(&target.bucket, &key, Vec::new())
                .await
                .map(|_| format!("Created {}", uri));
            (result, refresh)
        });
        Ok(())
    } else {
        editing::begin_create_object(state, &target, key)
    }
}

pub(super) fn start_rename(state: &mut AppState) {
    let Some(item) = state.selected_item().cloned() else {
        return;
    };
    match item.item_type {
        ItemType::Object | ItemType::Prefix => {
            let buffer = item.name.clone();
            open_input(state, InputPurpose::Rename { item }, buffer);
        }
        ItemType::Bucket => state.set_status(
            "Buckets can't be renamed; set a label for it in the config file instead".to_string(),
        ),
        ItemType::LoadMore => {}
    }
}

/// One item to move: copy every source key to its destination, then delete
/// the sources that copied successfully.
struct MovePlan {
    item: S3Item,
    destination_bucket: Arc<BucketEntry>,
    destination_key: String,
}

fn execute_moves(state: &mut AppState, plans: Vec<MovePlan>, verb: &'static str) {
    let client = state.client.clone();
    let progress = Progress(state.task_tx.clone());
    let mut refresh: Vec<Location> = Vec::new();
    for plan in &plans {
        if let Some(parent) = navigation::item_parent_location(&plan.item) {
            refresh.push(parent);
        }
        refresh.push(Location::new(
            plan.destination_bucket.clone(),
            parent_prefix(&plan.destination_key),
        ));
    }
    let count = plans.len();
    spawn_operation(
        state,
        format!("{} {}...", verb, counted_noun(count, "item", "items")),
        async move {
            let result = async {
                let mut jobs = Vec::new();
                let mut skipped = 0usize;
                for plan in &plans {
                    progress.report(format!("Listing {}...", plan.item.full_path()));
                    let objects = expand_item(&client, &plan.item).await?;
                    let existing: HashSet<String> = client
                        .list_recursive(&plan.destination_bucket, &plan.destination_key)
                        .await?
                        .into_iter()
                        .map(|object| object.key)
                        .collect();
                    for object in objects {
                        let suffix = object
                            .key
                            .strip_prefix(plan.item.key.as_str())
                            .unwrap_or("")
                            .to_string();
                        let destination_key = format!("{}{}", plan.destination_key, suffix);
                        if existing.contains(&destination_key) {
                            skipped += 1;
                            continue;
                        }
                        jobs.push((
                            plan.item.bucket.clone(),
                            object.key,
                            plan.destination_bucket.clone(),
                            destination_key,
                        ));
                    }
                }

                // Copy first; only delete sources whose copy succeeded.
                let copied = Arc::new(std::sync::Mutex::new(
                    Vec::<(Arc<BucketEntry>, String)>::new(),
                ));
                let mut outcome = run_bounded(
                    jobs,
                    &progress,
                    "Copying",
                    |(source, key, destination, destination_key)| {
                        let client = client.clone();
                        let copied = copied.clone();
                        async move {
                            client
                                .copy(&source, &key, &destination, &destination_key)
                                .await?;
                            copied
                                .lock()
                                .expect("copied list poisoned")
                                .push((source, key));
                            Ok(())
                        }
                    },
                )
                .await;
                outcome.skipped += skipped;

                let copied = std::mem::take(&mut *copied.lock().expect("copied list poisoned"));
                let mut by_bucket = KeysByBucket::new();
                for (bucket, key) in copied {
                    by_bucket
                        .entry((bucket.name.clone(), bucket.profile.clone()))
                        .or_insert_with(|| (bucket.clone(), Vec::new()))
                        .1
                        .push(key);
                }
                for (bucket, keys) in by_bucket.into_values() {
                    progress.report(format!("Removing {} originals...", keys.len()));
                    if let Err(err) = client.delete_keys(&bucket, keys).await {
                        outcome.errors.push(format!("{:#}", err));
                    }
                }
                let summary = format!(
                    "{} {}",
                    verb,
                    counted_noun(outcome.done, "object", "objects")
                );
                outcome.finish(summary, "destination already exists")
            }
            .await;
            (result, refresh)
        },
    );
}

fn execute_rename(state: &mut AppState, item: S3Item, new_name: &str) -> Result<()> {
    let new_name = new_name.trim().trim_start_matches('/');
    if new_name.is_empty() {
        return Err(anyhow!("Name is empty"));
    }
    let parent = parent_prefix(&item.key);
    let mut destination_key = format!("{}{}", parent, new_name);
    if item.item_type == ItemType::Prefix && !destination_key.ends_with('/') {
        destination_key.push('/');
    }
    if destination_key == item.key {
        state.set_status("Name unchanged".to_string());
        return Ok(());
    }
    if item.item_type == ItemType::Prefix && destination_key.starts_with(&item.key) {
        return Err(anyhow!("Can't move a prefix into itself"));
    }
    let destination_bucket = item.bucket.clone();
    execute_moves(
        state,
        vec![MovePlan {
            item,
            destination_bucket,
            destination_key,
        }],
        "Renamed",
    );
    Ok(())
}

pub(super) fn cut_selection(state: &mut AppState) {
    let items: Vec<S3Item> = state
        .target_items()
        .into_iter()
        .filter(|item| matches!(item.item_type, ItemType::Object | ItemType::Prefix))
        .collect();
    if items.is_empty() {
        state.set_status("Select objects or prefixes to cut".to_string());
        return;
    }
    state.set_status(format!(
        "Cut {}; open the destination and press p to move them there",
        counted_noun(items.len(), "item", "items")
    ));
    state.cut_items = items;
    state.selected_items.clear();
}

pub(super) fn paste_selection(state: &mut AppState) {
    if state.cut_items.is_empty() {
        state.set_status("Nothing to paste; cut items with x first".to_string());
        return;
    }
    let Some(destination) = state.active_location() else {
        state.set_status("Open a bucket or prefix to paste into".to_string());
        return;
    };
    let mut plans = Vec::new();
    for item in std::mem::take(&mut state.cut_items) {
        let destination_key = format!("{}{}", destination.prefix, key_basename(&item.key));
        let same_bucket = item.bucket.name == destination.bucket.name;
        if same_bucket && destination_key == item.key {
            continue;
        }
        if same_bucket
            && item.item_type == ItemType::Prefix
            && destination.prefix.starts_with(&item.key)
        {
            state.set_status(format!("Can't move {} into itself", item.full_path()));
            return;
        }
        plans.push(MovePlan {
            item,
            destination_bucket: destination.bucket.clone(),
            destination_key,
        });
    }
    if plans.is_empty() {
        state.set_status("Items are already here".to_string());
        return;
    }
    execute_moves(state, plans, "Moved");
}

pub(super) fn start_delete(state: &mut AppState) {
    let items = state.target_items();
    if items.is_empty() {
        return;
    }
    let target = if state.active_location().is_none() {
        let configured: Vec<S3Item> = items
            .into_iter()
            .filter(|item| item.bucket.is_configured())
            .collect();
        if configured.is_empty() {
            state.set_status(
                "Only buckets from your config file can be removed (hopper never deletes buckets)"
                    .to_string(),
            );
            return;
        }
        DeleteTarget::ConfigEntries(configured)
    } else {
        DeleteTarget::Items(
            items
                .into_iter()
                .filter(|item| matches!(item.item_type, ItemType::Object | ItemType::Prefix))
                .collect(),
        )
    };
    state.delete_target = Some(target);
    if state.delete_danger_mode {
        confirm_delete(state);
    } else {
        state.mode = AppMode::DeleteConfirm;
    }
}

pub(super) fn confirm_delete(state: &mut AppState) {
    state.mode = AppMode::Normal;
    let Some(target) = state.delete_target.take() else {
        return;
    };
    state.selected_items.clear();
    match target {
        DeleteTarget::ConfigEntries(items) => {
            let mut removed = 0usize;
            for item in &items {
                let BucketSource::Config(path) = &item.bucket.source else {
                    continue;
                };
                match remove_bucket_from_file(path, &item.bucket) {
                    Ok(true) => {
                        removed += 1;
                        state
                            .config
                            .buckets
                            .retain(|entry| !entry.same_target(&item.bucket));
                    }
                    Ok(false) => state.set_status(format!(
                        "{} not found in {}",
                        item.bucket.name,
                        path.display()
                    )),
                    Err(err) => {
                        state.set_status(format!("Failed to update {}: {:#}", path.display(), err));
                        return;
                    }
                }
            }
            refresh_root_level(state);
            if removed > 0 {
                state.set_status(format!(
                    "Removed {} from config (the buckets themselves are untouched)",
                    counted_noun(removed, "bucket", "buckets")
                ));
            }
        }
        DeleteTarget::Items(items) => execute_delete(state, items),
    }
}

fn execute_delete(state: &mut AppState, items: Vec<S3Item>) {
    let client = state.client.clone();
    let progress = Progress(state.task_tx.clone());
    let refresh: Vec<Location> = items
        .iter()
        .filter_map(navigation::item_parent_location)
        .collect();
    spawn_operation(state, "Deleting...".to_string(), async move {
        let result = async {
            let mut by_bucket = KeysByBucket::new();
            for item in &items {
                progress.report(format!("Listing {}...", item.full_path()));
                let objects = expand_item(&client, item).await?;
                let entry = by_bucket
                    .entry((item.bucket.name.clone(), item.bucket.profile.clone()))
                    .or_insert_with(|| (item.bucket.clone(), Vec::new()));
                entry.1.extend(objects.into_iter().map(|object| object.key));
            }
            let mut deleted = 0usize;
            for (bucket, keys) in by_bucket.into_values() {
                progress.report(format!("Deleting {} from {}...", keys.len(), bucket.name));
                let count = keys.len();
                client.delete_keys(&bucket, keys).await?;
                deleted += count;
            }
            Ok(format!(
                "Deleted {}",
                counted_noun(deleted, "object", "objects")
            ))
        }
        .await;
        (result, refresh)
    });
}

pub(super) fn submit_bucket_form(state: &mut AppState) -> Result<()> {
    let [bucket, profile, prefix, region, label] = state
        .bucket_form
        .values
        .clone()
        .map(|value| value.trim().to_string());
    let Some((name, spec_prefix)) = parse_bucket_spec(&bucket) else {
        return Err(anyhow!("Enter a bucket name"));
    };
    let path = state
        .config_write_path
        .clone()
        .ok_or_else(|| anyhow!("No config file location is available on this system"))?;
    let entry = BucketEntry {
        name,
        profile: (!profile.is_empty()).then_some(profile),
        region: (!region.is_empty()).then_some(region),
        prefix: if prefix.is_empty() {
            spec_prefix
        } else {
            normalize_prefix(&prefix)
        },
        label: (!label.is_empty()).then_some(label),
        source: BucketSource::Config(path.clone()),
    };
    if state
        .config
        .buckets
        .iter()
        .any(|existing| existing.same_target(&entry))
    {
        return Err(anyhow!("{} is already configured", entry.display_name()));
    }
    append_bucket_to_file(&path, &entry)?;
    state.config.add_bucket(entry.clone());
    state.mode = AppMode::Normal;
    refresh_root_level(state);

    let added = S3Item::bucket(Arc::new(entry.clone()));
    if let Some(index) = state
        .navigation
        .get_level(0)
        .and_then(|root| root.items.iter().position(|item| same_item(item, &added)))
        && state.window_start == 0
        && let Some(list_state) = state.list_states.get_mut(0)
    {
        list_state.select_item_index(index);
    }
    state.set_status(format!(
        "Added {} to {}",
        entry.display_name(),
        path.display()
    ));
    Ok(())
}

/// Dispatches Enter in the single-line prompt.
pub(super) fn submit_text_input(state: &mut AppState) -> Result<()> {
    let Some(input) = state.text_input.take() else {
        state.mode = AppMode::Normal;
        return Ok(());
    };
    state.mode = AppMode::Normal;
    let result = match input.purpose.clone() {
        InputPurpose::Download { items } => {
            execute_download(state, items, &input.buffer);
            Ok(())
        }
        InputPurpose::Upload { target } => {
            execute_upload(state, target, &input.buffer);
            Ok(())
        }
        InputPurpose::Create { target } => execute_create(state, target, &input.buffer),
        InputPurpose::Rename { item } => execute_rename(state, item, &input.buffer),
    };
    if let Err(err) = result {
        // Keep the prompt open so a typo can be fixed.
        state.set_status(format!("{:#}", err));
        state.text_input = Some(input);
        state.mode = AppMode::Input;
    }
    Ok(())
}

pub(super) fn copy_path(state: &mut AppState) {
    let path = state.get_current_path();
    if path.is_empty() {
        return;
    }
    if state.clipboard.is_none() {
        state.clipboard = arboard::Clipboard::new().ok();
    }
    match state.clipboard.as_mut() {
        Some(clipboard) => match clipboard.set_text(path.clone()) {
            Ok(()) => state.set_status(format!("Copied: {}", path)),
            Err(err) => state.set_status(format!("Failed to copy: {}", err)),
        },
        None => state.set_status("Failed to access clipboard".to_string()),
    }
}

fn encode_query_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{:02X}", byte)),
        }
    }
    encoded
}

/// AWS console URL for the selected item, or the active location.
pub(super) fn console_url(state: &AppState) -> Option<String> {
    let (bucket, key, is_object) = match state.selected_item() {
        Some(item) if item.item_type == ItemType::Object => {
            (item.bucket.clone(), item.key.clone(), true)
        }
        Some(item) if item.is_container() => (item.bucket.clone(), item.key.clone(), false),
        _ => {
            let location = state.active_location()?;
            (location.bucket, location.prefix, false)
        }
    };
    let region = state
        .client
        .known_region(&bucket)
        .map(|region| format!("region={}&", region))
        .unwrap_or_default();
    let kind = if is_object { "object" } else { "buckets" };
    Some(format!(
        "https://s3.console.aws.amazon.com/s3/{}/{}?{}prefix={}",
        kind,
        bucket.name,
        region,
        encode_query_value(&key)
    ))
}

pub(super) fn open_in_browser(state: &mut AppState) {
    let Some(url) = console_url(state) else {
        return;
    };
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let spawned = std::process::Command::new(opener)
        .arg(&url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match spawned {
        Ok(_) => state.set_status(format!("Opened {}", url)),
        Err(_) => state.set_status(format!("Open this URL: {}", url)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(prefix: &str) -> Location {
        Location::new(Arc::new(BucketEntry::new("b")), prefix)
    }

    fn summary(key: &str) -> ObjectSummary {
        ObjectSummary {
            key: key.to_string(),
            size: 1,
            last_modified: None,
            storage_class: None,
            etag: None,
        }
    }

    #[test]
    fn plan_download_recreates_prefix_folder_and_markers() {
        let item = S3Item::prefix(&location("data/"), "data/2024/".to_string());
        let objects = vec![
            summary("data/2024/"),
            summary("data/2024/a.csv"),
            summary("data/2024/q1/b.csv"),
        ];
        let (files, directories, rejected) = plan_download(&item, &objects, Path::new("/dl"));
        assert_eq!(
            files,
            vec![
                (
                    "data/2024/a.csv".to_string(),
                    PathBuf::from("/dl/2024/a.csv")
                ),
                (
                    "data/2024/q1/b.csv".to_string(),
                    PathBuf::from("/dl/2024/q1/b.csv")
                ),
            ]
        );
        assert_eq!(directories, vec![PathBuf::from("/dl/2024")]);
        assert!(rejected.is_empty());
    }

    #[test]
    fn plan_download_rejects_keys_that_escape_the_destination() {
        let item = S3Item::prefix(&location(""), "x/".to_string());
        let (files, _, rejected) =
            plan_download(&item, &[summary("x/../../etc/passwd")], Path::new("/dl"));
        assert!(files.is_empty());
        assert_eq!(rejected, vec!["x/../../etc/passwd".to_string()]);
    }

    #[test]
    fn plan_download_of_object_uses_its_file_name() {
        let item = S3Item::object(&location("a/"), summary("a/b.txt"));
        let (files, _, _) = plan_download(&item, &[summary("a/b.txt")], Path::new("/dl"));
        assert_eq!(
            files,
            vec![("a/b.txt".to_string(), PathBuf::from("/dl/b.txt"))]
        );
    }

    #[test]
    fn complete_local_path_extends_unique_and_common_prefixes() {
        let dir = std::env::temp_dir().join(format!("hopper-complete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("reports")).unwrap();
        std::fs::write(dir.join("report-a.csv"), "").unwrap();
        std::fs::write(dir.join("report-b.csv"), "").unwrap();
        std::fs::write(dir.join("unique.txt"), "").unwrap();
        let base = format!("{}/", dir.display());

        let (buffer, hint) = complete_local_path(&format!("{}un", base));
        assert_eq!(buffer, format!("{}unique.txt", base));
        assert!(hint.is_none());

        let (buffer, hint) = complete_local_path(&format!("{}rep", base));
        assert_eq!(buffer, format!("{}report", base));
        assert!(hint.unwrap().contains("reports/"));

        let (buffer, hint) = complete_local_path(&format!("{}reports", base));
        assert_eq!(buffer, format!("{}reports/", base));
        assert!(hint.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_upload_files_walks_directories_with_relative_keys() {
        let dir = std::env::temp_dir().join(format!("hopper-upload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("batch/nested")).unwrap();
        std::fs::write(dir.join("batch/one.txt"), "1").unwrap();
        std::fs::write(dir.join("batch/nested/two.txt"), "2").unwrap();
        let mut keys: Vec<String> = collect_upload_files(&dir.join("batch"))
            .unwrap()
            .into_iter()
            .map(|(_, key)| key)
            .collect();
        keys.sort();
        assert_eq!(keys, vec!["batch/nested/two.txt", "batch/one.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
