use crate::app::config::Config;
use crate::client::{
    BucketEntry, ListPage, Location, NavigationStack, ObjectPreview, S3Client, S3Item,
};
use anyhow::Result;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Normal,
    Search,
    Help,
    /// Single-line prompt (rename, download destination, upload source, new object).
    Input,
    /// Multi-field form for adding a bucket to the config.
    BucketForm,
    DeleteConfirm,
    DeleteDangerConfirm,
}

impl AppMode {
    pub fn is_text_input(self) -> bool {
        matches!(self, AppMode::Search | AppMode::Input | AppMode::BucketForm)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputPurpose {
    Rename { item: S3Item },
    Download { items: Vec<S3Item> },
    Upload { target: Location },
    Create { target: Location },
}

impl InputPurpose {
    pub fn title(&self) -> &'static str {
        match self {
            InputPurpose::Rename { .. } => " Rename ",
            InputPurpose::Download { .. } => " Download ",
            InputPurpose::Upload { .. } => " Upload ",
            InputPurpose::Create { .. } => " New Object ",
        }
    }

    /// Whether the buffer is a local path (enables Tab completion).
    pub fn is_local_path(&self) -> bool {
        matches!(
            self,
            InputPurpose::Download { .. } | InputPurpose::Upload { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextInput {
    pub purpose: InputPurpose,
    pub buffer: String,
    /// Shown under the input, e.g. ambiguous Tab completions.
    pub hint: Option<String>,
}

pub const BUCKET_FORM_FIELDS: [&str; 5] = ["Bucket", "Profile", "Prefix", "Region", "Label"];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BucketForm {
    pub values: [String; 5],
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeleteTarget {
    /// Objects and prefixes (prefixes are deleted recursively).
    Items(Vec<S3Item>),
    /// Configured buckets to remove from the config (the buckets themselves
    /// are untouched).
    ConfigEntries(Vec<S3Item>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PreviewState {
    Loading,
    Loaded(Box<ObjectPreview>),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PrefixSizeState {
    Loading,
    Loaded { objects: u64, bytes: u64 },
    Failed(String),
}

#[derive(Debug, Clone)]
pub enum ListingKind {
    /// Opening `item` from level `parent_level`; pushes a new level.
    Open {
        parent_level: usize,
        selected_index: usize,
        item: S3Item,
        location: Location,
    },
    /// Appending the next page to level `level`.
    LoadMore { level: usize, location: Location },
    /// Re-listing level `level` in place.
    Refresh { level: usize, location: Location },
}

/// Results that background tasks send back to the event loop.
#[derive(Debug)]
pub enum TaskResult {
    Listing {
        request_id: u64,
        kind: ListingKind,
        result: Result<ListPage>,
    },
    Discovered {
        profile: Option<String>,
        result: Result<Vec<String>>,
    },
    Preview {
        path: String,
        result: Result<ObjectPreview>,
    },
    PrefixSize {
        path: String,
        result: Result<(u64, u64)>,
    },
    Progress(String),
    OperationDone {
        result: Result<String>,
        /// Locations whose listings should be refreshed.
        refresh: Vec<Location>,
    },
    /// An object was downloaded for editing; the event loop opens the editor.
    EditReady(Result<EditRequest>),
}

/// A local copy of an object (or a new object) to open in `$EDITOR`.
#[derive(Debug, Clone)]
pub struct EditRequest {
    pub bucket: Arc<BucketEntry>,
    pub key: String,
    pub path: PathBuf,
    /// Contents before editing; `None` for a new object.
    pub original: Option<Vec<u8>>,
}

pub struct ListState {
    pub items: Vec<S3Item>,
    pub filtered_items: Vec<usize>,
    pub selected: usize,
    pub scroll_offset: usize,
}

impl ListState {
    pub fn new(items: Vec<S3Item>) -> Self {
        let filtered_items: Vec<usize> = (0..items.len()).collect();
        Self {
            items,
            filtered_items,
            selected: 0,
            scroll_offset: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.filtered_items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.filtered_items.is_empty()
    }

    pub fn selected_item(&self) -> Option<&S3Item> {
        let idx = *self.filtered_items.get(self.selected)?;
        self.items.get(idx)
    }

    /// Index into `items` of the cursor row.
    pub fn selected_item_index(&self) -> Option<usize> {
        self.filtered_items.get(self.selected).copied()
    }

    pub fn select_next(&mut self) {
        let len = self.filtered_items.len();
        if len == 0 {
            return;
        }
        self.selected = (self.selected + 1) % len;
    }

    pub fn select_previous(&mut self) {
        let len = self.filtered_items.len();
        if len == 0 {
            return;
        }
        self.selected = if self.selected == 0 {
            len - 1
        } else {
            self.selected - 1
        };
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        if !self.filtered_items.is_empty() {
            self.selected = self.filtered_items.len() - 1;
        }
    }

    pub fn page_down(&mut self, page_size: usize) {
        if !self.filtered_items.is_empty() {
            self.selected = (self.selected + page_size).min(self.filtered_items.len() - 1);
        }
    }

    pub fn page_up(&mut self, page_size: usize) {
        self.selected = self.selected.saturating_sub(page_size);
    }

    /// Moves the cursor to the row showing `items[item_index]`, if visible.
    pub fn select_item_index(&mut self, item_index: usize) -> bool {
        match self.filtered_items.iter().position(|&i| i == item_index) {
            Some(pos) => {
                self.selected = pos;
                true
            }
            None => false,
        }
    }

    pub fn filter(&mut self, query: &str) {
        let previous_selected_item = self.filtered_items.get(self.selected).copied();

        let query_lower = query.to_lowercase();
        self.filtered_items = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                query_lower.is_empty() || item.name.to_lowercase().contains(&query_lower)
            })
            .map(|(i, _)| i)
            .collect();

        if self.filtered_items.is_empty() {
            self.selected = 0;
            return;
        }

        if let Some(prev_index) = previous_selected_item
            && self.select_item_index(prev_index)
        {
            return;
        }

        if self.selected >= self.filtered_items.len() {
            self.selected = 0;
        }
    }
}

pub struct AppState {
    pub client: S3Client,
    pub config: Config,
    /// Where new bucket entries are written.
    pub config_write_path: Option<PathBuf>,
    /// Buckets given with `-b` on the command line.
    pub cli_buckets: Vec<BucketEntry>,
    /// Discovered bucket names per profile.
    pub discovered: BTreeMap<Option<String>, Vec<String>>,
    pub discovery_pending: usize,

    pub navigation: NavigationStack,
    pub list_states: Vec<ListState>,
    pub active_pane: usize,
    pub window_start: usize, // Track where the sliding window begins
    /// The `window_start` that `list_states` were built for, so cursors sync
    /// back to the right levels even after `window_start` has moved.
    pub list_states_window_start: usize,
    pub mode: AppMode,
    pub search_query: String,
    pub status_message: Option<String>,
    pub spinner_index: usize,
    pub help_scroll: usize,
    pub preview_scroll: usize,

    /// Items marked with Space, keyed by full path.
    pub selected_items: BTreeMap<String, S3Item>,
    /// Items cut with `x`, waiting for `p`.
    pub cut_items: Vec<S3Item>,
    pub delete_danger_mode: bool,
    pub delete_target: Option<DeleteTarget>,
    pub text_input: Option<TextInput>,
    pub bucket_form: BucketForm,

    pub previews: HashMap<String, PreviewState>,
    pub prefix_sizes: HashMap<String, PrefixSizeState>,
    /// Object under the cursor and when it got there; its preview is fetched
    /// once the cursor rests on it briefly.
    pub preview_candidate: Option<(S3Item, Instant)>,

    pub clipboard: Option<arboard::Clipboard>,
    pub should_quit: bool,
    pub needs_redraw: bool,

    /// Bumped whenever the user moves, so a slow listing for an item they
    /// have since moved away from is dropped instead of yanking focus.
    pub navigation_request: u64,
    pub pending_navigation: Option<(String, JoinHandle<()>)>,
    pub running_operations: usize,
    pub operation_progress: Option<String>,

    pub task_tx: UnboundedSender<TaskResult>,
    pub task_rx: UnboundedReceiver<TaskResult>,
}

impl AppState {
    pub fn new(client: S3Client, config: Config) -> Self {
        let (task_tx, task_rx) = unbounded_channel();
        Self {
            client,
            config,
            config_write_path: None,
            cli_buckets: Vec::new(),
            discovered: BTreeMap::new(),
            discovery_pending: 0,
            navigation: NavigationStack::new(),
            list_states: Vec::new(),
            active_pane: 0,
            window_start: 0,
            list_states_window_start: 0,
            mode: AppMode::Normal,
            search_query: String::new(),
            status_message: None,
            spinner_index: 0,
            help_scroll: 0,
            preview_scroll: 0,
            selected_items: BTreeMap::new(),
            cut_items: Vec::new(),
            delete_danger_mode: false,
            delete_target: None,
            text_input: None,
            bucket_form: BucketForm::default(),
            previews: HashMap::new(),
            prefix_sizes: HashMap::new(),
            preview_candidate: None,
            clipboard: None,
            should_quit: false,
            needs_redraw: false,
            navigation_request: 0,
            pending_navigation: None,
            running_operations: 0,
            operation_progress: None,
            task_tx,
            task_rx,
        }
    }

    pub fn visible_pane_count(&self) -> usize {
        // Fixed at 4 navigation panes (plus preview pane on right)
        4
    }

    pub fn active_level_index(&self) -> usize {
        self.window_start + self.active_pane
    }

    pub fn active_level(&self) -> Option<&crate::client::NavigationLevel> {
        self.navigation.get_level(self.active_level_index())
    }

    pub fn active_list_state(&self) -> Option<&ListState> {
        self.list_states.get(self.active_pane)
    }

    pub fn active_list_state_mut(&mut self) -> Option<&mut ListState> {
        self.list_states.get_mut(self.active_pane)
    }

    pub fn selected_item(&self) -> Option<&S3Item> {
        self.active_list_state()?.selected_item()
    }

    /// Location listed by the active pane (`None` at the bucket list).
    pub fn active_location(&self) -> Option<Location> {
        self.active_level()?.location.clone()
    }

    /// Marked items if any, otherwise the item under the cursor.
    pub fn target_items(&self) -> Vec<S3Item> {
        if !self.selected_items.is_empty() {
            return self.selected_items.values().cloned().collect();
        }
        self.selected_item()
            .filter(|item| item.item_type != crate::client::ItemType::LoadMore)
            .cloned()
            .into_iter()
            .collect()
    }

    pub fn is_busy(&self) -> bool {
        self.busy_message().is_some()
    }

    pub fn busy_message(&self) -> Option<String> {
        if let Some((label, _)) = &self.pending_navigation {
            return Some(format!("Loading {}...", label));
        }
        if self.running_operations > 0 {
            return Some(
                self.operation_progress
                    .clone()
                    .unwrap_or_else(|| "Working...".to_string()),
            );
        }
        if self.discovery_pending > 0 {
            return Some("Listing buckets...".to_string());
        }
        None
    }

    pub fn set_status(&mut self, message: String) {
        self.status_message = Some(message);
    }

    pub fn clear_status(&mut self) {
        self.status_message = None;
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Path shown in the status bar: the item under the cursor, else the
    /// active level's location.
    pub fn get_current_path(&self) -> String {
        if let Some(item) = self.selected_item()
            && item.item_type != crate::client::ItemType::LoadMore
        {
            return item.full_path();
        }
        self.active_location()
            .map(|location| location.uri())
            .unwrap_or_default()
    }

    /// All bucket entries for the root level: configured, then command line,
    /// then discovered (skipping names already shown with the same profile).
    pub fn root_bucket_entries(&self) -> Vec<Arc<BucketEntry>> {
        let mut entries: Vec<BucketEntry> = Vec::new();
        for entry in self.config.buckets.iter().chain(self.cli_buckets.iter()) {
            if !entries.iter().any(|existing| existing.same_target(entry)) {
                entries.push(entry.clone());
            }
        }
        for (profile, names) in &self.discovered {
            let mut names = names.clone();
            names.sort();
            for name in names {
                let effective = |entry: &BucketEntry| {
                    entry
                        .profile
                        .clone()
                        .or_else(|| self.config.default_profile.clone())
                };
                let discovered_profile = profile
                    .clone()
                    .or_else(|| self.config.default_profile.clone());
                let already_listed = entries
                    .iter()
                    .any(|entry| entry.name == name && effective(entry) == discovered_profile);
                if !already_listed {
                    entries.push(
                        BucketEntry::new(name)
                            .with_profile(profile.clone())
                            .with_source(crate::client::BucketSource::Discovered),
                    );
                }
            }
        }
        entries.into_iter().map(Arc::new).collect()
    }
}
