use super::*;

pub(super) const ROOT_TITLE: &str = "Buckets";

pub(super) fn initialize_app_impl(state: &mut AppState) {
    let root = NavigationLevel::new(ROOT_TITLE, root_items(state));
    state.navigation.push(root);
    rebuild_list_states(state);
    start_bucket_discovery(state);

    if state.root_bucket_entries().is_empty() && state.discovery_pending == 0 {
        state.set_status(no_buckets_hint(state));
    }
}

fn no_buckets_hint(state: &AppState) -> String {
    match &state.config_write_path {
        Some(path) => format!(
            "No buckets yet. Press a to add one (saved to {}), or edit that file.",
            path.display()
        ),
        None => "No buckets yet. Pass -b <bucket> or add buckets to config.toml.".to_string(),
    }
}

pub(super) fn root_items(state: &AppState) -> Vec<S3Item> {
    state
        .root_bucket_entries()
        .into_iter()
        .map(S3Item::bucket)
        .collect()
}

pub(super) fn start_bucket_discovery(state: &mut AppState) {
    for profile in state.config.discover.clone() {
        state.discovery_pending += 1;
        let client = state.client.clone();
        let tx = state.task_tx.clone();
        tokio::spawn(async move {
            let result = client.discover_buckets(profile.as_deref()).await;
            let _ = tx.send(TaskResult::Discovered { profile, result });
        });
    }
}

/// Rebuilds the root bucket list in place, keeping the cursor on the same
/// bucket when it is still there.
pub(super) fn refresh_root_level(state: &mut AppState) {
    let items = root_items(state);
    if let Some(root) = state.navigation.get_level_mut(0) {
        root.items = items;
    }
    rebuild_list_states(state);
}

/// Two items are the same entry if path and profile match (the same bucket
/// can be listed under two profiles).
pub(super) fn same_item(a: &S3Item, b: &S3Item) -> bool {
    a.item_type == b.item_type
        && a.bucket.name == b.bucket.name
        && a.bucket.profile == b.bucket.profile
        && a.key == b.key
}

pub(super) fn same_location(a: &Location, b: &Location) -> bool {
    a.bucket.name == b.bucket.name && a.bucket.profile == b.bucket.profile && a.prefix == b.prefix
}

/// The location containing `item` (the bucket list has none).
pub fn item_parent_location(item: &S3Item) -> Option<Location> {
    let parent_prefix = match item.item_type {
        ItemType::Bucket => return None,
        ItemType::LoadMore => item.key.clone(),
        ItemType::Prefix | ItemType::Object => parent_prefix(&item.key),
    };
    Some(Location::new(item.bucket.clone(), parent_prefix))
}

/// `a/b/c.txt` -> `a/b/`, `a/b/` -> `a/`, `top.txt` -> ``.
pub(super) fn parent_prefix(key: &str) -> String {
    let trimmed = key.strip_suffix('/').unwrap_or(key);
    match trimmed.rfind('/') {
        Some(slash) => trimmed[..=slash].to_string(),
        None => String::new(),
    }
}

/// Last path segment of a key, keeping the trailing slash for prefixes.
pub(super) fn key_basename(key: &str) -> String {
    let is_prefix = key.ends_with('/');
    let trimmed = key.strip_suffix('/').unwrap_or(key);
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if is_prefix {
        format!("{}/", name)
    } else {
        name.to_string()
    }
}

fn sync_visible_navigation_selection_from_list_states(state: &mut AppState) {
    for (pane_index, list_state) in state.list_states.iter().enumerate() {
        let level_index = state.list_states_window_start + pane_index;
        let selected = list_state.selected_item().cloned();
        let selected_index = list_state.selected_item_index();

        let Some(level) = state.navigation.get_level_mut(level_index) else {
            continue;
        };

        // Match by identity first so a refreshed level keeps the cursor on
        // the same entry even if its position changed.
        level.selected_index = selected
            .as_ref()
            .and_then(|selected| {
                level
                    .items
                    .iter()
                    .position(|item| same_item(item, selected))
            })
            .or_else(|| {
                selected_index
                    .filter(|_| !level.items.is_empty())
                    .map(|idx| idx.min(level.items.len() - 1))
            });
        level.selected_item = level
            .selected_index
            .and_then(|idx| level.items.get(idx).cloned());
    }
}

pub(super) fn rebuild_list_states(state: &mut AppState) {
    sync_visible_navigation_selection_from_list_states(state);

    let max_panes = state.visible_pane_count();
    let total_levels = state.navigation.levels_len();

    if total_levels == 0 {
        state.list_states.clear();
        state.window_start = 0;
        state.list_states_window_start = 0;
        state.active_pane = 0;
        return;
    }

    // Maintain a sliding window that only moves when necessary, always
    // keeping the active level visible.
    let active_level_index = (state.window_start + state.active_pane).min(total_levels - 1);
    if total_levels <= max_panes {
        state.window_start = 0;
    } else {
        if active_level_index < state.window_start {
            state.window_start = active_level_index;
        } else if active_level_index >= state.window_start + max_panes {
            state.window_start = active_level_index + 1 - max_panes;
        }
        state.window_start = state.window_start.min(total_levels - max_panes);
    }

    let window_end = (state.window_start + max_panes).min(total_levels);
    let query = if state.mode == AppMode::Search {
        state.search_query.clone()
    } else {
        String::new()
    };
    state.list_states = state.navigation.levels()[state.window_start..window_end]
        .iter()
        .enumerate()
        .map(|(offset, level)| {
            let mut list_state = ListState::new(level.items.clone());
            if let Some(selected) = level.selected_index {
                list_state.selected = selected.min(list_state.len().saturating_sub(1));
            }
            if state.window_start + offset == active_level_index && !query.is_empty() {
                list_state.filter(&query);
            }
            list_state
        })
        .collect();
    state.list_states_window_start = state.window_start;

    state.active_pane = active_level_index
        .saturating_sub(state.window_start)
        .min(state.list_states.len().saturating_sub(1));
}

/// Restores a pane's cursor to the entry whose children the next pane shows.
pub(super) fn restore_level_selection(state: &mut AppState, pane_index: usize) {
    let actual_level = state.window_start + pane_index;
    let selected_index = state
        .navigation
        .get_level(actual_level)
        .and_then(|level| level.selected_index);

    let Some(list_state) = state.list_states.get_mut(pane_index) else {
        return;
    };
    if let Some(item_index) = selected_index
        && list_state.select_item_index(item_index)
    {
        return;
    }
    list_state.selected = 0;
}

/// Cancels a pending open so its result cannot move focus after the user
/// has moved on.
pub(super) fn invalidate_navigation_requests(state: &mut AppState) {
    state.navigation_request = state.navigation_request.wrapping_add(1);
    if let Some((_, handle)) = state.pending_navigation.take() {
        handle.abort();
    }
}

fn spawn_listing(state: &mut AppState, label: String, kind: ListingKind, token: Option<String>) {
    invalidate_navigation_requests(state);
    let request_id = state.navigation_request;
    let client = state.client.clone();
    let tx = state.task_tx.clone();
    let location = match &kind {
        ListingKind::Open { location, .. }
        | ListingKind::LoadMore { location, .. }
        | ListingKind::Refresh { location, .. } => location.clone(),
    };
    let handle = tokio::spawn(async move {
        let result = client.list(&location.bucket, &location.prefix, token).await;
        let _ = tx.send(TaskResult::Listing {
            request_id,
            kind,
            result,
        });
    });
    state.pending_navigation = Some((label, handle));
}

pub(super) async fn select_current_item(state: &mut AppState) -> Result<()> {
    let (item, selected_index) = {
        let Some(list_state) = state.active_list_state() else {
            return Ok(());
        };
        let Some(item) = list_state.selected_item() else {
            return Ok(());
        };
        let Some(index) = list_state.selected_item_index() else {
            return Ok(());
        };
        (item.clone(), index)
    };
    let level_index = state.active_level_index();

    match item.item_type {
        ItemType::Bucket | ItemType::Prefix => {
            let Some(location) = item.child_location() else {
                return Ok(());
            };
            // Already open in the next pane: just move focus there.
            let already_open = state
                .navigation
                .get_level(level_index + 1)
                .and_then(|level| level.location.as_ref())
                .is_some_and(|open| same_location(open, &location))
                && state
                    .navigation
                    .get_level(level_index)
                    .and_then(|level| level.selected_index)
                    == Some(selected_index);
            if already_open {
                state.active_pane += 1;
                rebuild_list_states(state);
                return Ok(());
            }
            spawn_listing(
                state,
                location.uri(),
                ListingKind::Open {
                    parent_level: level_index,
                    selected_index,
                    item,
                    location,
                },
                None,
            );
        }
        ItemType::LoadMore => {
            let Some(level) = state.navigation.get_level(level_index) else {
                return Ok(());
            };
            let (Some(location), Some(token)) = (level.location.clone(), level.next_token.clone())
            else {
                return Ok(());
            };
            spawn_listing(
                state,
                format!("more of {}", location.uri()),
                ListingKind::LoadMore {
                    level: level_index,
                    location,
                },
                Some(token),
            );
        }
        ItemType::Object => {
            editing::begin_edit_object(state, item);
        }
    }
    Ok(())
}

pub(super) fn go_back(state: &mut AppState) {
    if state.navigation.levels_len() > 1 {
        let old_active_level = state.active_level_index();

        state.navigation.pop();

        // Keep the active level aligned to the immediate parent after pop.
        let new_total = state.navigation.levels_len();
        let target_active_level = old_active_level
            .saturating_sub(1)
            .min(new_total.saturating_sub(1));
        state.window_start = 0;
        state.active_pane = target_active_level;

        rebuild_list_states(state);
    }
}

/// Re-lists the active level (the root re-runs bucket discovery).
pub(super) fn refresh_current_level(state: &mut AppState) {
    let level_index = state.active_level_index();
    let location = state
        .navigation
        .get_level(level_index)
        .and_then(|level| level.location.clone());
    match location {
        None => {
            state.discovered.clear();
            refresh_root_level(state);
            start_bucket_discovery(state);
            if state.discovery_pending == 0 {
                state.set_status("Refreshed".to_string());
            }
        }
        Some(location) => {
            state.previews.clear();
            spawn_refresh(state, level_index, location);
        }
    }
}

fn spawn_refresh(state: &mut AppState, level: usize, location: Location) {
    // Refreshes don't go through `spawn_listing`: they must not cancel (or be
    // cancelled by) an open, and several may run at once after an operation.
    let client = state.client.clone();
    let tx = state.task_tx.clone();
    let request_id = state.navigation_request;
    tokio::spawn(async move {
        let result = client.list(&location.bucket, &location.prefix, None).await;
        let _ = tx.send(TaskResult::Listing {
            request_id,
            kind: ListingKind::Refresh { level, location },
            result,
        });
    });
}

/// Refreshes every open level that lists one of `locations`.
pub(super) fn refresh_locations(state: &mut AppState, locations: &[Location]) {
    let targets: Vec<(usize, Location)> = state
        .navigation
        .levels()
        .iter()
        .enumerate()
        .filter_map(|(index, level)| {
            let location = level.location.as_ref()?;
            locations
                .iter()
                .any(|target| same_location(target, location))
                .then(|| (index, location.clone()))
        })
        .collect();
    for (level, location) in targets {
        spawn_refresh(state, level, location);
    }
}

fn level_title(item: &S3Item) -> String {
    match item.item_type {
        ItemType::Bucket => item.bucket.display_name(),
        _ => item.name.clone(),
    }
}

fn apply_listing_result(
    state: &mut AppState,
    request_id: u64,
    kind: ListingKind,
    result: Result<ListPage>,
) {
    match kind {
        ListingKind::Open {
            parent_level,
            selected_index,
            item,
            location,
        } => {
            if request_id != state.navigation_request {
                return;
            }
            state.pending_navigation = None;
            let page = match result {
                Ok(page) => page,
                Err(err) => {
                    state.set_status(format!("Open failed: {:#}", err));
                    return;
                }
            };
            if parent_level >= state.navigation.levels_len() {
                return;
            }
            sync_visible_navigation_selection_from_list_states(state);
            state.navigation.truncate(parent_level + 1);
            if let Some(parent) = state.navigation.get_level_mut(parent_level) {
                parent.selected_index = Some(selected_index);
                parent.selected_item = Some(item.clone());
            }
            state.navigation.push(NavigationLevel::from_page(
                level_title(&item),
                location,
                page,
            ));
            state.active_pane = (parent_level + 1).saturating_sub(state.window_start);
            rebuild_list_states(state);
            state.preview_scroll = 0;
        }
        ListingKind::LoadMore { level, location } => {
            if request_id != state.navigation_request {
                return;
            }
            state.pending_navigation = None;
            let page = match result {
                Ok(page) => page,
                Err(err) => {
                    state.set_status(format!("Load more failed: {:#}", err));
                    return;
                }
            };
            let Some(target) = level_at_location(state, level, &location) else {
                return;
            };
            if target
                .items
                .last()
                .is_some_and(|item| item.item_type == ItemType::LoadMore)
            {
                target.items.pop();
            }
            target.next_token = page.next_token.clone();
            let has_more = target.next_token.is_some();
            target.items.extend(page.into_items(&location));
            if has_more {
                target.items.push(S3Item::load_more(&location));
            }
            // The cursor was on the removed "load more" row, so the sync falls
            // back to its index: the first newly loaded entry.
            rebuild_list_states(state);
        }
        ListingKind::Refresh { level, location } => {
            let page = match result {
                Ok(page) => page,
                Err(err) => {
                    state.set_status(format!("Refresh failed: {:#}", err));
                    return;
                }
            };
            let next_prefix = state
                .navigation
                .get_level(level + 1)
                .and_then(|next| next.location.as_ref())
                .map(|next| next.prefix.clone());
            let Some(target) = level_at_location(state, level, &location) else {
                return;
            };
            let refreshed = NavigationLevel::from_page(target.title.clone(), location, page);
            target.items = refreshed.items;
            target.next_token = refreshed.next_token;

            // Drop deeper panes whose prefix disappeared.
            let next_still_exists = next_prefix.is_none_or(|prefix| {
                target
                    .items
                    .iter()
                    .any(|item| item.item_type == ItemType::Prefix && item.key == prefix)
            });
            if !next_still_exists {
                state.navigation.truncate(level + 1);
                if state.active_level_index() > level {
                    state.active_pane = level.saturating_sub(state.window_start);
                }
            }
            // The sync in rebuild keeps each cursor on the same entry.
            rebuild_list_states(state);
        }
    }
}

/// The level at `index` if it still lists `location`.
fn level_at_location<'a>(
    state: &'a mut AppState,
    index: usize,
    location: &Location,
) -> Option<&'a mut NavigationLevel> {
    state.navigation.get_level_mut(index).filter(|level| {
        level
            .location
            .as_ref()
            .is_some_and(|open| same_location(open, location))
    })
}

pub(super) async fn poll_background_tasks_impl(state: &mut AppState) -> Result<()> {
    while let Ok(result) = state.task_rx.try_recv() {
        match result {
            TaskResult::Listing {
                request_id,
                kind,
                result,
            } => apply_listing_result(state, request_id, kind, result),
            TaskResult::Discovered { profile, result } => {
                state.discovery_pending = state.discovery_pending.saturating_sub(1);
                match result {
                    Ok(names) => {
                        state.discovered.insert(profile, names);
                        refresh_root_level(state);
                    }
                    Err(err) => state.set_status(format!("Bucket discovery failed: {:#}", err)),
                }
                if state.discovery_pending == 0 && state.root_bucket_entries().is_empty() {
                    state.set_status(no_buckets_hint(state));
                }
            }
            TaskResult::Preview { path, result } => {
                let entry = match result {
                    Ok(preview) => PreviewState::Loaded(Box::new(preview)),
                    Err(err) => PreviewState::Failed(format!("{:#}", err)),
                };
                state.previews.insert(path, entry);
            }
            TaskResult::PrefixSize { path, result } => {
                let entry = match result {
                    Ok((objects, bytes)) => {
                        state.set_status(format!(
                            "{}: {} in {}",
                            path,
                            crate::ui::helpers::format_size(bytes),
                            operations::counted_noun(objects as usize, "object", "objects")
                        ));
                        PrefixSizeState::Loaded { objects, bytes }
                    }
                    Err(err) => {
                        state.set_status(format!("Size failed: {:#}", err));
                        PrefixSizeState::Failed(format!("{:#}", err))
                    }
                };
                state.prefix_sizes.insert(path, entry);
            }
            TaskResult::Progress(message) => {
                if state.running_operations > 0 {
                    state.operation_progress = Some(message);
                }
            }
            TaskResult::OperationDone { result, refresh } => {
                state.running_operations = state.running_operations.saturating_sub(1);
                if state.running_operations == 0 {
                    state.operation_progress = None;
                }
                match result {
                    Ok(message) => state.set_status(message),
                    Err(err) => state.set_status(format!("{:#}", err)),
                }
                state.previews.clear();
                state.prefix_sizes.clear();
                refresh_locations(state, &refresh);
            }
            TaskResult::EditReady(result) => match result {
                Ok(request) => editing::run_editor(state, request).await?,
                Err(err) => {
                    state.running_operations = state.running_operations.saturating_sub(1);
                    if state.running_operations == 0 {
                        state.operation_progress = None;
                    }
                    state.set_status(format!("Open failed: {:#}", err));
                }
            },
        }
    }

    preview::maybe_fetch_candidate_preview(state);
    Ok(())
}
