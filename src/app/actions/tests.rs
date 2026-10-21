use super::*;
use crate::app::config::{Config, Viewer, append_bucket_to_file, load_config};
use crate::client::mock::MockStore;
use std::path::PathBuf;
use std::time::Duration;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hopper-actions-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn store() -> MockStore {
    let mut store = MockStore::empty();
    store.add_bucket("alpha", None, true);
    store.add_object("alpha", "readme.txt", "hello\nworld\n");
    store.add_object("alpha", "data/2024/jan.csv", "a,b\n1,2\n");
    store.add_object("alpha", "data/2024/feb.csv", "a,b\n3,4\n");
    store.add_object("alpha", "data/2023/dec.csv", "a,b\n5,6\n");
    store.add_object("alpha", "deep/a/b/c/d/leaf.txt", "leaf");
    for i in 0..1250 {
        store.add_object("alpha", &format!("many/f{:04}", i), "x");
    }
    store.add_bucket("beta", None, true);
    store.add_object("beta", "inbox/keep.txt", "keep");
    store.add_bucket("private", Some("partner"), false);
    store.add_object("private", "drop/file.csv", "secret");
    store
}

/// A config file with one configured bucket, so remove-from-config works.
fn config_with(entries: &[BucketEntry], name: &str) -> (Config, PathBuf) {
    let path = temp_dir(name).join("config.toml");
    for entry in entries {
        append_bucket_to_file(&path, entry).unwrap();
    }
    if entries.is_empty() {
        std::fs::write(&path, "").unwrap();
    }
    (load_config(Some(&path)).unwrap(), path)
}

/// Applies background results until nothing has arrived for a while.
async fn settle(state: &mut AppState) {
    let mut idle = 0;
    for _ in 0..400 {
        poll_background_tasks(state).await.unwrap();
        if state.is_busy() {
            idle = 0;
        } else {
            idle += 1;
            if idle >= 8 {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn new_state(config: Config, config_path: Option<PathBuf>) -> AppState {
    let mut state = AppState::new(S3Client::mock_with(store()), config);
    state.config_write_path = config_path;
    initialize_app(&mut state);
    settle(&mut state).await;
    state
}

async fn press(state: &mut AppState, event: AppEvent) {
    handle_event(state, event).await.unwrap();
    settle(state).await;
}

fn pane_names(state: &AppState, pane: usize) -> Vec<String> {
    let list_state = &state.list_states[pane];
    list_state
        .filtered_items
        .iter()
        .map(|&idx| list_state.items[idx].name.clone())
        .collect()
}

fn active_names(state: &AppState) -> Vec<String> {
    pane_names(state, state.active_pane)
}

fn select_named(state: &mut AppState, name: &str) {
    let list_state = state.active_list_state_mut().unwrap();
    let index = list_state
        .items
        .iter()
        .position(|item| item.name == name)
        .unwrap_or_else(|| panic!("no item named {}", name));
    assert!(
        list_state.select_item_index(index),
        "{} is filtered out",
        name
    );
}

async fn open(state: &mut AppState, name: &str) {
    select_named(state, name);
    press(state, AppEvent::SelectItem).await;
}

#[tokio::test]
async fn root_lists_configured_then_discovered_without_duplicates() {
    let (config, path) = config_with(
        &[
            BucketEntry::new("private").with_profile(Some("partner".into())),
            BucketEntry::new("beta"),
        ],
        "root",
    );
    let state = new_state(config, Some(path)).await;
    // beta is configured and discovered with the same profile: listed once.
    assert_eq!(active_names(&state), vec!["private", "beta", "alpha"]);
    let root = &state.list_states[0].items;
    assert!(root[0].bucket.is_configured());
    assert_eq!(root[2].bucket.source, BucketSource::Discovered);
}

#[tokio::test]
async fn opening_buckets_and_prefixes_builds_miller_columns() {
    let (config, path) = config_with(&[], "open");
    let mut state = new_state(config, Some(path)).await;

    open(&mut state, "alpha").await;
    assert_eq!(state.navigation.levels_len(), 2);
    assert_eq!(state.active_pane, 1);
    assert_eq!(
        active_names(&state),
        vec!["data/", "deep/", "many/", "readme.txt"]
    );

    open(&mut state, "data/").await;
    assert_eq!(active_names(&state), vec!["2023/", "2024/"]);
    assert_eq!(state.get_current_path(), "s3://alpha/data/2023/");

    // h moves focus left without discarding the deeper pane; Enter on a
    // different prefix replaces it.
    press(&mut state, AppEvent::MoveLeft).await;
    assert_eq!(state.active_pane, 1);
    assert_eq!(state.navigation.levels_len(), 3);
    open(&mut state, "deep/").await;
    assert_eq!(state.navigation.levels_len(), 3);
    assert_eq!(active_names(&state), vec!["a/"]);

    press(&mut state, AppEvent::GoBack).await;
    assert_eq!(state.navigation.levels_len(), 2);
    assert_eq!(state.active_pane, 1);
}

#[tokio::test]
async fn sliding_window_keeps_active_level_visible_and_cursors_on_their_levels() {
    let (config, path) = config_with(&[], "slide");
    let mut state = new_state(config, Some(path)).await;
    open(&mut state, "alpha").await;
    open(&mut state, "deep/").await;
    open(&mut state, "a/").await;
    open(&mut state, "b/").await;
    open(&mut state, "c/").await;
    assert_eq!(state.navigation.levels_len(), 6);
    assert_eq!(state.window_start, 2);
    assert_eq!(state.active_pane, 3);
    assert_eq!(active_names(&state), vec!["d/"]);

    // Walk left past the window edge, then back right.
    for _ in 0..5 {
        press(&mut state, AppEvent::MoveLeft).await;
    }
    assert_eq!(state.window_start, 0);
    assert_eq!(state.active_pane, 0);
    // The root cursor must still be on alpha (not clobbered by sliding).
    assert_eq!(state.selected_item().unwrap().name, "alpha");
    for _ in 0..5 {
        press(&mut state, AppEvent::MoveRight).await;
    }
    assert_eq!(state.active_level_index(), 5);
    assert_eq!(state.selected_item().unwrap().name, "d/");
    let deep_level = state.navigation.get_level(1).unwrap();
    assert_eq!(
        deep_level.items[deep_level.selected_index.unwrap()].name,
        "deep/"
    );
}

#[tokio::test]
async fn load_more_appends_the_next_page_and_keeps_cursor_on_new_entries() {
    let (config, path) = config_with(&[], "more");
    let mut state = new_state(config, Some(path)).await;
    open(&mut state, "alpha").await;
    open(&mut state, "many/").await;
    let names = active_names(&state);
    assert_eq!(names.len(), 1001);
    assert_eq!(names.last().unwrap(), "Load more...");

    press(&mut state, AppEvent::GoToBottom).await;
    press(&mut state, AppEvent::SelectItem).await;
    let names = active_names(&state);
    assert_eq!(names.len(), 1250);
    assert!(!names.contains(&"Load more...".to_string()));
    assert_eq!(state.selected_item().unwrap().name, "f1000");
}

#[tokio::test]
async fn access_denied_reports_error_and_profile_grants_access() {
    let (config, path) = config_with(
        &[
            BucketEntry::new("private"),
            BucketEntry::new("private").with_profile(Some("partner".into())),
        ],
        "denied",
    );
    let mut state = new_state(config, Some(path)).await;

    state.active_list_state_mut().unwrap().selected = 0;
    press(&mut state, AppEvent::SelectItem).await;
    assert_eq!(state.navigation.levels_len(), 1);
    assert!(
        state
            .status_message
            .as_deref()
            .unwrap()
            .contains("AccessDenied"),
        "{:?}",
        state.status_message
    );

    state.active_list_state_mut().unwrap().selected = 1;
    press(&mut state, AppEvent::SelectItem).await;
    assert_eq!(state.navigation.levels_len(), 2);
    assert_eq!(active_names(&state), vec!["drop/"]);
}

#[tokio::test]
async fn moving_before_a_listing_returns_drops_the_stale_result() {
    let (config, path) = config_with(&[], "stale");
    let mut state = new_state(config, Some(path)).await;
    select_named(&mut state, "alpha");
    handle_event(&mut state, AppEvent::SelectItem)
        .await
        .unwrap();
    handle_event(&mut state, AppEvent::MoveDown).await.unwrap();
    settle(&mut state).await;
    assert_eq!(state.navigation.levels_len(), 1);
}

#[tokio::test]
async fn search_filters_and_enter_opens_the_match() {
    let (config, path) = config_with(&[], "search");
    let mut state = new_state(config, Some(path)).await;
    open(&mut state, "alpha").await;

    press(&mut state, AppEvent::StartSearch).await;
    for c in "ma".chars() {
        press(&mut state, AppEvent::InputChar(c)).await;
    }
    assert_eq!(active_names(&state), vec!["many/"]);
    press(&mut state, AppEvent::InputSubmit).await;
    assert_eq!(state.mode, AppMode::Normal);
    assert_eq!(state.navigation.levels_len(), 3);
    // The parent pane is unfiltered again with its cursor on the match.
    assert_eq!(pane_names(&state, 1).len(), 4);
    let parent = state.navigation.get_level(1).unwrap();
    assert_eq!(parent.items[parent.selected_index.unwrap()].name, "many/");
}

#[tokio::test]
async fn preview_is_fetched_once_the_cursor_rests_on_an_object() {
    let (config, path) = config_with(&[], "preview");
    let mut state = new_state(config, Some(path)).await;
    open(&mut state, "alpha").await;
    select_named(&mut state, "readme.txt");
    press(&mut state, AppEvent::PreviewScrollUp).await; // any event notes the selection
    tokio::time::sleep(Duration::from_millis(150)).await;
    settle(&mut state).await;
    match state.previews.get("s3://alpha/readme.txt") {
        Some(PreviewState::Loaded(preview)) => {
            assert_eq!(
                preview.body,
                PreviewBody::Text {
                    text: "hello\nworld\n".to_string(),
                    truncated: false
                }
            );
        }
        other => panic!("unexpected preview state {:?}", other),
    }
}

#[tokio::test]
async fn delete_asks_for_confirmation_and_removes_prefix_recursively() {
    let (config, path) = config_with(&[], "delete");
    let mut state = new_state(config, Some(path)).await;
    open(&mut state, "alpha").await;
    open(&mut state, "data/").await;
    select_named(&mut state, "2024/");

    press(&mut state, AppEvent::DeleteItem).await;
    assert_eq!(state.mode, AppMode::DeleteConfirm);
    press(&mut state, AppEvent::CancelDelete).await;
    assert_eq!(active_names(&state), vec!["2023/", "2024/"]);

    select_named(&mut state, "2024/");
    press(&mut state, AppEvent::DeleteItem).await;
    press(&mut state, AppEvent::ConfirmDelete).await;
    assert_eq!(state.status_message.as_deref(), Some("Deleted 2 objects"));
    assert_eq!(active_names(&state), vec!["2023/"]);
}

#[tokio::test]
async fn rename_object_and_reject_existing_destination() {
    let (config, path) = config_with(&[], "rename");
    let mut state = new_state(config, Some(path)).await;
    open(&mut state, "alpha").await;
    open(&mut state, "data/").await;
    open(&mut state, "2024/").await;
    select_named(&mut state, "jan.csv");

    press(&mut state, AppEvent::RenameItem).await;
    assert_eq!(state.mode, AppMode::Input);
    press(&mut state, AppEvent::InputClear).await;
    for c in "january.csv".chars() {
        press(&mut state, AppEvent::InputChar(c)).await;
    }
    press(&mut state, AppEvent::InputSubmit).await;
    assert_eq!(active_names(&state), vec!["feb.csv", "january.csv"]);

    // Renaming onto an existing key is skipped, not overwritten.
    select_named(&mut state, "feb.csv");
    press(&mut state, AppEvent::RenameItem).await;
    press(&mut state, AppEvent::InputClear).await;
    for c in "january.csv".chars() {
        press(&mut state, AppEvent::InputChar(c)).await;
    }
    press(&mut state, AppEvent::InputSubmit).await;
    assert!(
        state
            .status_message
            .as_deref()
            .unwrap()
            .contains("1 skipped"),
        "{:?}",
        state.status_message
    );
    assert_eq!(active_names(&state), vec!["feb.csv", "january.csv"]);
}

#[tokio::test]
async fn cut_and_paste_moves_a_prefix_across_buckets() {
    let (config, path) = config_with(&[], "move");
    let mut state = new_state(config, Some(path)).await;
    open(&mut state, "alpha").await;
    open(&mut state, "data/").await;
    select_named(&mut state, "2023/");
    press(&mut state, AppEvent::CutSelection).await;
    assert_eq!(state.cut_items.len(), 1);

    // Back to the root, into beta/inbox/, paste.
    press(&mut state, AppEvent::GoBack).await;
    press(&mut state, AppEvent::GoBack).await;
    open(&mut state, "beta").await;
    open(&mut state, "inbox/").await;
    press(&mut state, AppEvent::PasteSelection).await;
    assert_eq!(state.status_message.as_deref(), Some("Moved 1 object"));
    assert_eq!(active_names(&state), vec!["2023/", "keep.txt"]);
    assert!(state.cut_items.is_empty());

    let source = state
        .client
        .list_recursive(&BucketEntry::new("alpha"), "data/2023/")
        .await
        .unwrap();
    assert!(source.is_empty());
}

#[tokio::test]
async fn upload_directory_then_download_prefix_round_trips() {
    let (config, path) = config_with(&[], "transfer");
    let mut state = new_state(config, Some(path)).await;
    let local = temp_dir("transfer-local");
    std::fs::create_dir_all(local.join("batch/nested")).unwrap();
    std::fs::write(local.join("batch/one.txt"), "1").unwrap();
    std::fs::write(local.join("batch/nested/two.txt"), "2").unwrap();

    open(&mut state, "beta").await;
    press(&mut state, AppEvent::Upload).await;
    state.text_input.as_mut().unwrap().buffer = format!("{}/batch", local.display());
    press(&mut state, AppEvent::InputSubmit).await;
    assert_eq!(
        state.status_message.as_deref(),
        Some("Uploaded 2 files to s3://beta/")
    );
    assert_eq!(active_names(&state), vec!["batch/", "inbox/"]);

    // Uploading again skips what already exists.
    press(&mut state, AppEvent::Upload).await;
    state.text_input.as_mut().unwrap().buffer = format!("{}/batch/", local.display());
    press(&mut state, AppEvent::InputSubmit).await;
    assert!(
        state
            .status_message
            .as_deref()
            .unwrap()
            .contains("2 skipped")
    );

    let destination = temp_dir("transfer-download");
    select_named(&mut state, "batch/");
    press(&mut state, AppEvent::Download).await;
    state.text_input.as_mut().unwrap().buffer = destination.display().to_string();
    press(&mut state, AppEvent::InputSubmit).await;
    assert_eq!(
        std::fs::read_to_string(destination.join("batch/nested/two.txt")).unwrap(),
        "2"
    );
    assert!(
        state
            .status_message
            .as_deref()
            .unwrap()
            .starts_with("Downloaded 2 objects")
    );
}

#[tokio::test]
async fn create_folder_marker_shows_up_as_empty_prefix() {
    let (config, path) = config_with(&[], "create");
    let mut state = new_state(config, Some(path)).await;
    open(&mut state, "beta").await;
    press(&mut state, AppEvent::CreateItem).await;
    assert_eq!(state.mode, AppMode::Input);
    for c in "fresh/".chars() {
        press(&mut state, AppEvent::InputChar(c)).await;
    }
    press(&mut state, AppEvent::InputSubmit).await;
    assert_eq!(active_names(&state), vec!["fresh/", "inbox/"]);
    open(&mut state, "fresh/").await;
    assert!(active_names(&state).is_empty());
}

#[tokio::test]
async fn add_bucket_form_writes_config_and_remove_deletes_the_entry() {
    let (config, path) = config_with(&[], "form");
    let mut state = new_state(config, Some(path.clone())).await;

    press(&mut state, AppEvent::CreateItem).await;
    assert_eq!(state.mode, AppMode::BucketForm);
    for c in "s3://private/drop".chars() {
        press(&mut state, AppEvent::InputChar(c)).await;
    }
    press(&mut state, AppEvent::MoveDown).await;
    for c in "partner".chars() {
        press(&mut state, AppEvent::InputChar(c)).await;
    }
    press(&mut state, AppEvent::InputSubmit).await;
    assert_eq!(state.mode, AppMode::Normal);
    assert_eq!(state.selected_item().unwrap().name, "private/drop/");

    let reloaded = load_config(Some(&path)).unwrap();
    assert_eq!(reloaded.buckets.len(), 1);
    assert_eq!(reloaded.buckets[0].profile.as_deref(), Some("partner"));
    assert_eq!(reloaded.buckets[0].prefix, "drop/");

    // The new entry opens straight into its prefix.
    press(&mut state, AppEvent::SelectItem).await;
    assert_eq!(active_names(&state), vec!["file.csv"]);
    press(&mut state, AppEvent::GoBack).await;

    // d at the root removes the config entry only.
    select_named(&mut state, "private/drop/");
    press(&mut state, AppEvent::DeleteItem).await;
    assert_eq!(state.mode, AppMode::DeleteConfirm);
    press(&mut state, AppEvent::ConfirmDelete).await;
    assert!(load_config(Some(&path)).unwrap().buckets.is_empty());
    assert_eq!(active_names(&state), vec!["alpha", "beta"]);

    // Discovered buckets can't be removed or deleted.
    select_named(&mut state, "alpha");
    press(&mut state, AppEvent::DeleteItem).await;
    assert_eq!(state.mode, AppMode::Normal);
    assert!(
        state
            .status_message
            .as_deref()
            .unwrap()
            .contains("never deletes buckets")
    );
}

#[tokio::test]
async fn danger_mode_requires_confirmation_then_skips_dialogs() {
    let (config, path) = config_with(&[], "danger");
    let mut state = new_state(config, Some(path)).await;
    open(&mut state, "alpha").await;

    press(&mut state, AppEvent::ToggleDeleteDangerMode).await;
    assert_eq!(state.mode, AppMode::DeleteDangerConfirm);
    press(&mut state, AppEvent::ConfirmEnableDeleteDangerMode).await;
    assert!(state.delete_danger_mode);

    select_named(&mut state, "readme.txt");
    press(&mut state, AppEvent::DeleteItem).await;
    assert_eq!(state.mode, AppMode::Normal);
    assert!(!active_names(&state).contains(&"readme.txt".to_string()));
}

#[tokio::test]
async fn marked_items_are_the_targets_of_operations() {
    let (config, path) = config_with(&[], "marks");
    let mut state = new_state(config, Some(path)).await;
    open(&mut state, "alpha").await;
    open(&mut state, "data/").await;
    open(&mut state, "2024/").await;
    state.active_list_state_mut().unwrap().selected = 0;
    press(&mut state, AppEvent::ToggleSelect).await;
    press(&mut state, AppEvent::ToggleSelect).await;
    assert_eq!(state.selected_items.len(), 2);

    press(&mut state, AppEvent::DeleteItem).await;
    match &state.delete_target {
        Some(DeleteTarget::Items(items)) => assert_eq!(items.len(), 2),
        other => panic!("unexpected target {:?}", other),
    }
    press(&mut state, AppEvent::ConfirmDelete).await;
    assert!(state.selected_items.is_empty());
    assert!(active_names(&state).is_empty());
}

#[tokio::test]
async fn refresh_drops_deeper_pane_whose_prefix_vanished() {
    let (config, path) = config_with(&[], "refresh");
    let mut state = new_state(config, Some(path)).await;
    open(&mut state, "alpha").await;
    open(&mut state, "data/").await;
    open(&mut state, "2023/").await;
    assert_eq!(state.navigation.levels_len(), 4);

    // Delete data/2023/ behind hopper's back, then refresh the data/ level.
    state
        .client
        .delete_keys(
            &BucketEntry::new("alpha"),
            vec!["data/2023/dec.csv".to_string()],
        )
        .await
        .unwrap();
    press(&mut state, AppEvent::MoveLeft).await;
    press(&mut state, AppEvent::Refresh).await;
    assert_eq!(active_names(&state), vec!["2024/"]);
    assert_eq!(state.navigation.levels_len(), 3);
}

#[tokio::test]
async fn view_downloads_object_and_runs_the_matching_viewer() {
    let (mut config, path) = config_with(&[], "view");
    config.viewers.push(Viewer {
        extensions: vec!["csv".to_string()],
        command: vec!["true".to_string()],
    });
    let mut state = new_state(config, Some(path)).await;

    open(&mut state, "alpha").await;
    select_named(&mut state, "readme.txt");
    press(&mut state, AppEvent::ViewObject).await;
    assert_eq!(
        state.status_message.as_deref(),
        Some("No viewer for .txt; add a [[viewers]] entry to your config")
    );

    select_named(&mut state, "data/");
    press(&mut state, AppEvent::ViewObject).await;
    assert_eq!(
        state.status_message.as_deref(),
        Some("Select an object to view")
    );

    open(&mut state, "data/").await;
    open(&mut state, "2024/").await;
    select_named(&mut state, "jan.csv");
    press(&mut state, AppEvent::ViewObject).await;
    assert_eq!(
        state.status_message.as_deref(),
        Some("Closed s3://alpha/data/2024/jan.csv")
    );
}
