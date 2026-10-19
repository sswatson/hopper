//! Loading and editing `config.toml`.
//!
//! ```toml
//! profile = "default"          # profile for buckets that don't name one
//! region = "us-east-1"         # region used before a bucket's own is known
//! discover = true              # also list ListBuckets results (or a list of profiles)
//!
//! buckets = [
//!   "my-team-bucket",
//!   "s3://shared-drop/vendor/acme/",
//!   { name = "other-account-bucket", profile = "legacy" },
//!   { name = "vendor-feed", profile = "legacy", prefix = "exports/", label = "Vendor exports" },
//! ]
//! ```

use crate::client::{BucketEntry, BucketSource, normalize_prefix, parse_bucket_spec};
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub default_profile: Option<String>,
    pub default_region: Option<String>,
    /// Profiles to run ListBuckets with at startup; `None` is the default
    /// profile. Empty disables discovery.
    pub discover: Vec<Option<String>>,
    pub buckets: Vec<BucketEntry>,
    pub loaded_files: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    profile: Option<String>,
    region: Option<String>,
    discover: Option<RawDiscover>,
    #[serde(default)]
    buckets: Vec<RawBucket>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawDiscover {
    Enabled(bool),
    Profiles(Vec<String>),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawBucket {
    Name(String),
    Table(RawBucketTable),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBucketTable {
    name: String,
    profile: Option<String>,
    region: Option<String>,
    prefix: Option<String>,
    label: Option<String>,
}

impl RawBucket {
    fn into_entry(self, path: &Path) -> Result<BucketEntry> {
        let (spec, profile, region, prefix, label) = match self {
            RawBucket::Name(name) => (name, None, None, None, None),
            RawBucket::Table(table) => (
                table.name,
                table.profile,
                table.region,
                table.prefix,
                table.label,
            ),
        };
        let (name, spec_prefix) = parse_bucket_spec(&spec)
            .ok_or_else(|| anyhow!("invalid bucket name {:?} in {}", spec, path.display()))?;
        Ok(BucketEntry {
            name,
            profile: non_empty(profile),
            region: non_empty(region),
            prefix: prefix
                .map(|prefix| normalize_prefix(&prefix))
                .unwrap_or(spec_prefix),
            label: non_empty(label),
            source: BucketSource::Config(path.to_path_buf()),
        })
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn global_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("hopper").join("config.toml"))
}

pub fn project_config_path(cwd: &Path) -> PathBuf {
    cwd.join(".config").join("hopper").join("config.toml")
}

/// The file that `a` (add bucket) writes to: an explicit `--config`, else the
/// global config.
pub fn writable_config_path(explicit: Option<&Path>) -> Option<PathBuf> {
    explicit.map(Path::to_path_buf).or_else(global_config_path)
}

/// Loads `--config` if given; otherwise the global config followed by the
/// project config in the current directory, with later files overriding
/// scalar settings and appending buckets.
pub fn load_config(explicit: Option<&Path>) -> Result<Config> {
    let paths = match explicit {
        Some(path) => {
            if !path.exists() {
                return Err(anyhow!("config file {} does not exist", path.display()));
            }
            vec![path.to_path_buf()]
        }
        None => {
            let mut paths = Vec::new();
            if let Some(global) = global_config_path() {
                paths.push(global);
            }
            if let Ok(cwd) = std::env::current_dir() {
                let project = project_config_path(&cwd);
                if !paths.contains(&project) {
                    paths.push(project);
                }
            }
            paths
        }
    };
    load_config_from_paths(&paths)
}

fn load_config_from_paths(paths: &[PathBuf]) -> Result<Config> {
    let mut config = Config {
        discover: vec![None],
        ..Default::default()
    };
    for path in paths {
        if !path.is_file() {
            continue;
        }
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let raw: RawConfig = toml::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        config.merge(raw, path)?;
        config.loaded_files.push(path.clone());
    }
    Ok(config)
}

impl Config {
    fn merge(&mut self, raw: RawConfig, path: &Path) -> Result<()> {
        if let Some(profile) = non_empty(raw.profile) {
            self.default_profile = Some(profile);
        }
        if let Some(region) = non_empty(raw.region) {
            self.default_region = Some(region);
        }
        if let Some(discover) = raw.discover {
            self.discover = match discover {
                RawDiscover::Enabled(true) => vec![None],
                RawDiscover::Enabled(false) => Vec::new(),
                RawDiscover::Profiles(profiles) => profiles.into_iter().map(Some).collect(),
            };
        }
        for bucket in raw.buckets {
            self.add_bucket(bucket.into_entry(path)?);
        }
        Ok(())
    }

    /// Adds a bucket unless an entry for the same bucket/profile/prefix exists.
    pub fn add_bucket(&mut self, entry: BucketEntry) -> bool {
        if self
            .buckets
            .iter()
            .any(|existing| existing.same_target(&entry))
        {
            return false;
        }
        self.buckets.push(entry);
        true
    }
}

/// Appends a bucket to the config file at `path`, creating the file if needed
/// and preserving existing comments and formatting.
pub fn append_bucket_to_file(path: &Path, entry: &BucketEntry) -> Result<()> {
    let contents = if path.exists() {
        std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?
    } else {
        String::new()
    };
    let mut doc: toml_edit::DocumentMut = contents
        .parse()
        .with_context(|| format!("failed to parse {}", path.display()))?;

    let has_details = entry.profile.is_some()
        || entry.region.is_some()
        || !entry.prefix.is_empty()
        || entry.label.is_some();

    match doc.get_mut("buckets") {
        Some(toml_edit::Item::ArrayOfTables(tables)) => {
            let mut table = toml_edit::Table::new();
            for (key, value) in bucket_fields(entry) {
                table.insert(key, toml_edit::value(value));
            }
            tables.push(table);
        }
        Some(item) => {
            let array = item
                .as_array_mut()
                .ok_or_else(|| anyhow!("`buckets` in {} is not an array", path.display()))?;
            push_bucket_value(array, entry, has_details);
        }
        None => {
            let mut array = toml_edit::Array::new();
            push_bucket_value(&mut array, entry, has_details);
            doc.insert("buckets", toml_edit::value(array));
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, doc.to_string())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn bucket_fields(entry: &BucketEntry) -> Vec<(&'static str, String)> {
    let mut fields = vec![("name", entry.name.clone())];
    if let Some(profile) = &entry.profile {
        fields.push(("profile", profile.clone()));
    }
    if let Some(region) = &entry.region {
        fields.push(("region", region.clone()));
    }
    if !entry.prefix.is_empty() {
        fields.push(("prefix", entry.prefix.clone()));
    }
    if let Some(label) = &entry.label {
        fields.push(("label", label.clone()));
    }
    fields
}

fn push_bucket_value(array: &mut toml_edit::Array, entry: &BucketEntry, has_details: bool) {
    let mut value: toml_edit::Value = if has_details {
        let mut table = toml_edit::InlineTable::new();
        for (key, value) in bucket_fields(entry) {
            table.insert(key, value.into());
        }
        table.into()
    } else {
        entry.name.clone().into()
    };
    // One bucket per line keeps the list easy to edit by hand. A comment after
    // the last element lives in the array's trailing text, so carry it over
    // in front of the new element rather than dropping it.
    let trailing = array.trailing().as_str().unwrap_or("").to_string();
    let prefix = if trailing.contains('\n') {
        format!("{}  ", trailing.trim_end_matches(' '))
    } else {
        format!("{}\n  ", trailing)
    };
    value.decor_mut().set_prefix(prefix);
    array.push_formatted(value);
    array.set_trailing("\n");
    array.set_trailing_comma(true);
}

/// Removes the entry for `entry` (same bucket, profile, and prefix) from the
/// config file it was loaded from.
pub fn remove_bucket_from_file(path: &Path, entry: &BucketEntry) -> Result<bool> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = contents
        .parse()
        .with_context(|| format!("failed to parse {}", path.display()))?;

    let matches = |name: &str, profile: Option<&str>, prefix: Option<&str>| -> bool {
        let Some((bucket, spec_prefix)) = parse_bucket_spec(name) else {
            return false;
        };
        let prefix = prefix.map(normalize_prefix).unwrap_or(spec_prefix);
        let profile = profile.map(str::trim).filter(|profile| !profile.is_empty());
        bucket == entry.name && profile == entry.profile.as_deref() && prefix == entry.prefix
    };

    let removed = match doc.get_mut("buckets") {
        Some(toml_edit::Item::ArrayOfTables(tables)) => {
            let position = tables.iter().position(|table| {
                matches(
                    table.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    table.get("profile").and_then(|v| v.as_str()),
                    table.get("prefix").and_then(|v| v.as_str()),
                )
            });
            if let Some(position) = position {
                tables.remove(position);
            }
            position.is_some()
        }
        Some(item) => match item.as_array_mut() {
            Some(array) => {
                let position = array.iter().position(|value| match value {
                    toml_edit::Value::String(name) => matches(name.value(), None, None),
                    toml_edit::Value::InlineTable(table) => matches(
                        table.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        table.get("profile").and_then(|v| v.as_str()),
                        table.get("prefix").and_then(|v| v.as_str()),
                    ),
                    _ => false,
                });
                if let Some(position) = position {
                    array.remove(position);
                }
                position.is_some()
            }
            None => false,
        },
        None => false,
    };

    if removed {
        std::fs::write(path, doc.to_string())
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hopper-config-test-{}-{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    #[test]
    fn parses_mixed_bucket_forms_and_settings() {
        let path = temp_path("parse");
        std::fs::write(
            &path,
            r#"
profile = "dev"
discover = ["dev", "legacy"]
buckets = [
  "plain",
  "s3://with-prefix/a/b",
  { name = "cross", profile = "legacy", region = "eu-west-1", label = "Cross account" },
  { name = "s3://feed/ignored", prefix = "real/" },
]
"#,
        )
        .unwrap();

        let config = load_config_from_paths(std::slice::from_ref(&path)).unwrap();
        assert_eq!(config.default_profile.as_deref(), Some("dev"));
        assert_eq!(
            config.discover,
            vec![Some("dev".to_string()), Some("legacy".to_string())]
        );
        let summary: Vec<(String, Option<String>, String)> = config
            .buckets
            .iter()
            .map(|b| (b.name.clone(), b.profile.clone(), b.prefix.clone()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("plain".to_string(), None, String::new()),
                ("with-prefix".to_string(), None, "a/b/".to_string()),
                (
                    "cross".to_string(),
                    Some("legacy".to_string()),
                    String::new()
                ),
                ("feed".to_string(), None, "real/".to_string()),
            ]
        );
        assert_eq!(config.buckets[2].region.as_deref(), Some("eu-west-1"));
        assert_eq!(config.buckets[2].display_name(), "Cross account");
    }

    #[test]
    fn example_config_in_repo_parses() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config.example.toml");
        let config = load_config_from_paths(&[path]).unwrap();
        assert_eq!(config.buckets.len(), 5);
        assert_eq!(config.buckets[1].prefix, "vendor/acme/");
        assert_eq!(config.buckets[3].display_name(), "Vendor exports");
    }

    #[test]
    fn missing_files_give_defaults_with_discovery_on() {
        let config =
            load_config_from_paths(&[temp_path("missing").with_file_name("nope.toml")]).unwrap();
        assert_eq!(config.discover, vec![None]);
        assert!(config.buckets.is_empty());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let path = temp_path("unknown");
        std::fs::write(&path, "bukets = [\"x\"]\n").unwrap();
        assert!(load_config_from_paths(&[path]).is_err());
    }

    #[test]
    fn later_files_append_buckets_and_skip_duplicates() {
        let global = temp_path("global");
        let project = temp_path("project");
        std::fs::write(&global, "buckets = [\"a\", \"b\"]\n").unwrap();
        std::fs::write(&project, "discover = false\nbuckets = [\"b\", \"c\"]\n").unwrap();
        let config = load_config_from_paths(&[global, project]).unwrap();
        let names: Vec<&str> = config.buckets.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        assert!(config.discover.is_empty());
    }

    #[test]
    fn append_and_remove_round_trip_preserving_comments() {
        let path = temp_path("edit");
        std::fs::write(
            &path,
            "# my buckets\nbuckets = [\n  \"keep\", # important\n]\n",
        )
        .unwrap();

        let entry = BucketEntry::new("added")
            .with_profile(Some("legacy".to_string()))
            .with_prefix("x/");
        append_bucket_to_file(&path, &entry).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("# my buckets"));
        assert!(written.contains("# important"));

        let config = load_config_from_paths(std::slice::from_ref(&path)).unwrap();
        assert_eq!(config.buckets.len(), 2);
        assert_eq!(config.buckets[1].profile.as_deref(), Some("legacy"));
        assert_eq!(config.buckets[1].prefix, "x/");

        assert!(remove_bucket_from_file(&path, &entry).unwrap());
        let config = load_config_from_paths(std::slice::from_ref(&path)).unwrap();
        let names: Vec<&str> = config.buckets.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["keep"]);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("# my buckets")
        );
    }

    #[test]
    fn append_creates_file_and_supports_array_of_tables() {
        let path = temp_path("create");
        append_bucket_to_file(&path, &BucketEntry::new("fresh")).unwrap();
        let config = load_config_from_paths(std::slice::from_ref(&path)).unwrap();
        assert_eq!(config.buckets[0].name, "fresh");

        let tables = temp_path("tables");
        std::fs::write(&tables, "[[buckets]]\nname = \"one\"\n").unwrap();
        append_bucket_to_file(
            &tables,
            &BucketEntry::new("two").with_profile(Some("p".into())),
        )
        .unwrap();
        let config = load_config_from_paths(std::slice::from_ref(&tables)).unwrap();
        assert_eq!(config.buckets.len(), 2);
        assert_eq!(config.buckets[1].profile.as_deref(), Some("p"));
    }
}
