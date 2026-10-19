use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Where a bucket entry came from. Configured entries can be removed from
/// their file; discovered ones only exist for the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BucketSource {
    Config(PathBuf),
    CommandLine,
    Discovered,
}

/// A bucket shown at the root level, together with how to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketEntry {
    pub name: String,
    /// AWS profile to use; `None` falls back to the default profile.
    pub profile: Option<String>,
    /// Region override; `None` means hopper asks S3 where the bucket lives.
    pub region: Option<String>,
    /// Prefix to open when entering the bucket (empty for the bucket root).
    pub prefix: String,
    pub label: Option<String>,
    pub source: BucketSource,
}

impl BucketEntry {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            profile: None,
            region: None,
            prefix: String::new(),
            label: None,
            source: BucketSource::CommandLine,
        }
    }

    pub fn with_profile(mut self, profile: Option<String>) -> Self {
        self.profile = profile;
        self
    }

    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = normalize_prefix(&prefix.into());
        self
    }

    pub fn with_source(mut self, source: BucketSource) -> Self {
        self.source = source;
        self
    }

    pub fn display_name(&self) -> String {
        if let Some(label) = &self.label {
            return label.clone();
        }
        if self.prefix.is_empty() {
            self.name.clone()
        } else {
            format!("{}/{}", self.name, self.prefix)
        }
    }

    pub fn is_configured(&self) -> bool {
        matches!(self.source, BucketSource::Config(_))
    }

    /// Two entries point at the same place if bucket, profile, and prefix match.
    pub fn same_target(&self, other: &BucketEntry) -> bool {
        self.name == other.name && self.profile == other.profile && self.prefix == other.prefix
    }
}

/// Turns user-typed prefixes into S3 form: no leading slash, trailing slash
/// unless empty.
pub fn normalize_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim().trim_start_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else if trimmed.ends_with('/') {
        trimmed.to_string()
    } else {
        format!("{}/", trimmed)
    }
}

/// Splits `s3://bucket/prefix` (or `bucket/prefix`) into bucket and prefix.
pub fn parse_bucket_spec(spec: &str) -> Option<(String, String)> {
    let rest = spec.trim().trim_start_matches("s3://");
    let (bucket, prefix) = match rest.split_once('/') {
        Some((bucket, prefix)) => (bucket, prefix),
        None => (rest, ""),
    };
    if bucket.is_empty() {
        return None;
    }
    Some((bucket.to_string(), normalize_prefix(prefix)))
}

/// A bucket plus a prefix within it: the place a navigation level lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub bucket: Arc<BucketEntry>,
    pub prefix: String,
}

impl Location {
    pub fn new(bucket: Arc<BucketEntry>, prefix: impl Into<String>) -> Self {
        Self {
            bucket,
            prefix: prefix.into(),
        }
    }

    pub fn uri(&self) -> String {
        format!("s3://{}/{}", self.bucket.name, self.prefix)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemType {
    Bucket,
    Prefix,
    Object,
    LoadMore,
}

impl ItemType {
    pub fn icon(&self) -> &'static str {
        match self {
            ItemType::Bucket => "🪣",
            ItemType::Prefix => "📁",
            ItemType::Object => "📄",
            ItemType::LoadMore => "↓",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct S3Item {
    /// Display name relative to the containing level.
    pub name: String,
    pub item_type: ItemType,
    pub bucket: Arc<BucketEntry>,
    /// Full key: the start prefix for buckets, `a/b/` for prefixes, the
    /// object key for objects, the listing prefix for "load more".
    pub key: String,
    pub size: Option<u64>,
    pub last_modified: Option<DateTime<Utc>>,
    pub storage_class: Option<String>,
    pub etag: Option<String>,
}

impl S3Item {
    pub fn bucket(entry: Arc<BucketEntry>) -> Self {
        Self {
            name: entry.display_name(),
            item_type: ItemType::Bucket,
            key: entry.prefix.clone(),
            bucket: entry,
            size: None,
            last_modified: None,
            storage_class: None,
            etag: None,
        }
    }

    pub fn prefix(location: &Location, prefix: String) -> Self {
        let name = prefix
            .strip_prefix(location.prefix.as_str())
            .unwrap_or(prefix.as_str())
            .to_string();
        Self {
            name,
            item_type: ItemType::Prefix,
            bucket: location.bucket.clone(),
            key: prefix,
            size: None,
            last_modified: None,
            storage_class: None,
            etag: None,
        }
    }

    pub fn object(location: &Location, object: ObjectSummary) -> Self {
        let name = object
            .key
            .strip_prefix(location.prefix.as_str())
            .unwrap_or(object.key.as_str())
            .to_string();
        Self {
            name,
            item_type: ItemType::Object,
            bucket: location.bucket.clone(),
            key: object.key,
            size: Some(object.size),
            last_modified: object.last_modified,
            storage_class: object.storage_class,
            etag: object.etag,
        }
    }

    pub fn load_more(location: &Location) -> Self {
        Self {
            name: "Load more...".to_string(),
            item_type: ItemType::LoadMore,
            bucket: location.bucket.clone(),
            key: location.prefix.clone(),
            size: None,
            last_modified: None,
            storage_class: None,
            etag: None,
        }
    }

    pub fn icon(&self) -> &'static str {
        self.item_type.icon()
    }

    pub fn full_path(&self) -> String {
        format!("s3://{}/{}", self.bucket.name, self.key)
    }

    /// The location this item opens into, if it is a container.
    pub fn child_location(&self) -> Option<Location> {
        match self.item_type {
            ItemType::Bucket | ItemType::Prefix => {
                Some(Location::new(self.bucket.clone(), self.key.clone()))
            }
            ItemType::Object | ItemType::LoadMore => None,
        }
    }

    pub fn is_container(&self) -> bool {
        matches!(self.item_type, ItemType::Bucket | ItemType::Prefix)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectSummary {
    pub key: String,
    pub size: u64,
    pub last_modified: Option<DateTime<Utc>>,
    pub storage_class: Option<String>,
    pub etag: Option<String>,
}

/// One page of a delimited listing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListPage {
    pub prefixes: Vec<String>,
    pub objects: Vec<ObjectSummary>,
    pub next_token: Option<String>,
}

impl ListPage {
    /// Converts the page into display items: prefixes first, then objects,
    /// skipping the zero-byte "folder marker" object named after the prefix
    /// itself.
    pub fn into_items(self, location: &Location) -> Vec<S3Item> {
        let mut items: Vec<S3Item> = self
            .prefixes
            .into_iter()
            .filter(|prefix| prefix != &location.prefix)
            .map(|prefix| S3Item::prefix(location, prefix))
            .collect();
        items.extend(
            self.objects
                .into_iter()
                .filter(|object| object.key != location.prefix)
                .map(|object| S3Item::object(location, object)),
        );
        items
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObjectDetails {
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    pub content_encoding: Option<String>,
    pub cache_control: Option<String>,
    pub last_modified: Option<DateTime<Utc>>,
    pub etag: Option<String>,
    pub storage_class: Option<String>,
    pub server_side_encryption: Option<String>,
    pub version_id: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PreviewBody {
    Text {
        text: String,
        truncated: bool,
    },
    Binary {
        description: String,
    },
    Empty,
    /// The body was not fetched (e.g. archived storage class).
    Skipped {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectPreview {
    pub details: ObjectDetails,
    pub body: PreviewBody,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NavigationLevel {
    pub title: String,
    pub items: Vec<S3Item>,
    /// Cursor position in this level (index into `items`).
    pub selected_index: Option<usize>,
    pub selected_item: Option<S3Item>,
    /// What this level lists; `None` for the root bucket list.
    pub location: Option<Location>,
    /// Continuation token for the next page, if the listing was truncated.
    pub next_token: Option<String>,
}

impl NavigationLevel {
    pub fn new(title: impl Into<String>, items: Vec<S3Item>) -> Self {
        Self {
            title: title.into(),
            items,
            ..Default::default()
        }
    }

    pub fn with_location(mut self, location: Location) -> Self {
        self.location = Some(location);
        self
    }

    /// Builds a level from a first listing page, appending a "load more"
    /// entry when the listing was truncated.
    pub fn from_page(title: impl Into<String>, location: Location, page: ListPage) -> Self {
        let next_token = page.next_token.clone();
        let mut items = page.into_items(&location);
        if next_token.is_some() {
            items.push(S3Item::load_more(&location));
        }
        Self {
            title: title.into(),
            items,
            selected_index: None,
            selected_item: None,
            location: Some(location),
            next_token,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_prefix_adds_trailing_slash_and_strips_leading() {
        assert_eq!(normalize_prefix(""), "");
        assert_eq!(normalize_prefix("/"), "");
        assert_eq!(normalize_prefix("a/b"), "a/b/");
        assert_eq!(normalize_prefix("/a/b/"), "a/b/");
    }

    #[test]
    fn parse_bucket_spec_accepts_uri_and_bare_forms() {
        assert_eq!(
            parse_bucket_spec("s3://my-bucket/data/2024"),
            Some(("my-bucket".to_string(), "data/2024/".to_string()))
        );
        assert_eq!(
            parse_bucket_spec("my-bucket"),
            Some(("my-bucket".to_string(), String::new()))
        );
        assert_eq!(parse_bucket_spec("s3://"), None);
    }

    #[test]
    fn list_page_items_hide_folder_marker_and_strip_prefix() {
        let location = Location::new(Arc::new(BucketEntry::new("b")), "data/");
        let page = ListPage {
            prefixes: vec!["data/2024/".to_string()],
            objects: vec![
                ObjectSummary {
                    key: "data/".to_string(),
                    size: 0,
                    last_modified: None,
                    storage_class: None,
                    etag: None,
                },
                ObjectSummary {
                    key: "data/readme.md".to_string(),
                    size: 12,
                    last_modified: None,
                    storage_class: None,
                    etag: None,
                },
            ],
            next_token: None,
        };
        let names: Vec<String> = page
            .into_items(&location)
            .into_iter()
            .map(|item| item.name)
            .collect();
        assert_eq!(names, vec!["2024/", "readme.md"]);
    }

    #[test]
    fn display_name_includes_start_prefix_unless_labelled() {
        let entry = BucketEntry::new("b").with_prefix("x/y");
        assert_eq!(entry.display_name(), "b/x/y/");
        let labelled = BucketEntry {
            label: Some("Vendor drop".to_string()),
            ..entry
        };
        assert_eq!(labelled.display_name(), "Vendor drop");
    }
}
