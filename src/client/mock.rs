use super::types::{BucketEntry, ListPage, ObjectDetails, ObjectSummary};
use anyhow::{Result, anyhow};
use chrono::{DateTime, Duration, TimeZone, Utc};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Debug, Clone)]
struct MockObject {
    data: Vec<u8>,
    last_modified: DateTime<Utc>,
    storage_class: String,
}

#[derive(Debug, Clone, Default)]
struct MockBucket {
    /// When set, only this profile may access the bucket (like a
    /// cross-account bucket that the default credentials cannot read).
    required_profile: Option<String>,
    objects: BTreeMap<String, MockObject>,
}

/// In-memory S3 used by `--demo` and the test suite.
#[derive(Debug, Default)]
pub struct MockStore {
    buckets: Mutex<BTreeMap<String, MockBucket>>,
    /// What ListBuckets returns, keyed by profile.
    discoverable: BTreeMap<Option<String>, Vec<String>>,
}

impl MockStore {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn add_bucket(&mut self, name: &str, required_profile: Option<&str>, discoverable: bool) {
        self.buckets.get_mut().expect("mock poisoned").insert(
            name.to_string(),
            MockBucket {
                required_profile: required_profile.map(str::to_string),
                objects: BTreeMap::new(),
            },
        );
        if discoverable {
            self.discoverable
                .entry(required_profile.map(str::to_string))
                .or_default()
                .push(name.to_string());
        }
    }

    pub fn add_object(&mut self, bucket: &str, key: &str, data: impl Into<Vec<u8>>) {
        self.add_object_with_class(bucket, key, data, "STANDARD");
    }

    pub fn add_object_with_class(
        &mut self,
        bucket: &str,
        key: &str,
        data: impl Into<Vec<u8>>,
        storage_class: &str,
    ) {
        let buckets = self.buckets.get_mut().expect("mock poisoned");
        let bucket = buckets.entry(bucket.to_string()).or_default();
        let offset = bucket.objects.len() as i64;
        bucket.objects.insert(
            key.to_string(),
            MockObject {
                data: data.into(),
                last_modified: Utc.with_ymd_and_hms(2024, 6, 1, 9, 30, 0).unwrap()
                    + Duration::minutes(offset * 17),
                storage_class: storage_class.to_string(),
            },
        );
    }

    /// Sample data for `--demo`.
    pub fn demo() -> Self {
        let mut store = Self::empty();

        store.add_bucket("acme-analytics", None, true);
        store.add_object(
            "acme-analytics",
            "README.md",
            "# acme-analytics\n\nCurated analytics outputs.\n\n- `reports/`: quarterly CSV reports\n- `events/`: raw event exports (JSON lines)\n- `logs/`: application logs\n",
        );
        for quarter in 1..=4 {
            store.add_object(
                "acme-analytics",
                &format!("reports/2024/q{}.csv", quarter),
                format!(
                    "region,revenue,units\nnorth,{},{}\nsouth,{},{}\neast,{},{}\nwest,{},{}\n",
                    1200 * quarter,
                    40 * quarter,
                    980 * quarter,
                    31 * quarter,
                    1430 * quarter,
                    52 * quarter,
                    760 * quarter,
                    25 * quarter
                ),
            );
        }
        store.add_object(
            "acme-analytics",
            "reports/2023/annual.csv",
            "region,revenue\nall,98000\n",
        );
        for part in 0..1250 {
            store.add_object(
                "acme-analytics",
                &format!("events/part-{:05}.json", part),
                format!("{{\"event\":\"click\",\"part\":{}}}\n", part),
            );
        }
        store.add_object(
            "acme-analytics",
            "logs/app/2024-06-01.log",
            "2024-06-01T09:30:00Z INFO  server started\n2024-06-01T09:31:12Z WARN  slow request path=/api/report ms=1830\n2024-06-01T09:40:55Z ERROR upstream timeout service=pricing\n",
        );
        store.add_object("acme-analytics", "logs/empty/", Vec::new());

        store.add_bucket("acme-ml-models", None, true);
        let mut weights = b"\x89WTS\x00\x01".to_vec();
        weights.extend((0..4096u32).map(|i| (i % 251) as u8));
        store.add_object("acme-ml-models", "models/ranker/v3/weights.bin", weights);
        store.add_object(
            "acme-ml-models",
            "models/ranker/v3/config.yaml",
            "model: ranker\nversion: 3\nfeatures:\n  - clicks_7d\n  - price\n  - category\nlearning_rate: 0.003\n",
        );

        // Not returned by ListBuckets, and only readable with the `partner`
        // profile: the case that motivates configuring buckets explicitly.
        store.add_bucket("partner-data-feed", Some("partner"), false);
        let mut parquet = b"PAR1".to_vec();
        parquet.extend(std::iter::repeat_n(0u8, 2048));
        parquet.extend(b"PAR1");
        store.add_object(
            "partner-data-feed",
            "drops/2024-06-01/prices.parquet",
            parquet,
        );
        store.add_object(
            "partner-data-feed",
            "drops/2024-06-01/manifest.json",
            "{\n  \"files\": [\"prices.parquet\"],\n  \"rows\": 1048576\n}\n",
        );

        store.add_bucket("acme-archive", None, false);
        store.add_object_with_class(
            "acme-archive",
            "2023/backup.tar",
            vec![0u8; 8192],
            "DEEP_ARCHIVE",
        );
        store.add_object_with_class(
            "acme-archive",
            "2023/notes.txt",
            "archived notes\n",
            "GLACIER",
        );

        store
    }

    pub(super) fn effective_profile(&self, bucket: &BucketEntry) -> Option<String> {
        bucket.profile.clone()
    }

    fn with_bucket<T>(
        &self,
        entry: &BucketEntry,
        f: impl FnOnce(&mut MockBucket) -> Result<T>,
    ) -> Result<T> {
        let mut buckets = self.buckets.lock().map_err(|_| anyhow!("mock poisoned"))?;
        let bucket = buckets
            .get_mut(&entry.name)
            .ok_or_else(|| anyhow!("NoSuchBucket: The specified bucket does not exist"))?;
        if let Some(required) = &bucket.required_profile
            && entry.profile.as_ref() != Some(required)
        {
            return Err(anyhow!(
                "List s3://{}: AccessDenied: User is not authorized to perform: s3:ListBucket",
                entry.name
            ));
        }
        f(bucket)
    }

    pub(super) fn discover_buckets(&self, profile: Option<&str>) -> Result<Vec<String>> {
        Ok(self
            .discoverable
            .get(&profile.map(str::to_string))
            .cloned()
            .unwrap_or_default())
    }

    /// Delimited listing. The continuation token is the last entry (key or
    /// common prefix) returned, which works because entries come out in
    /// non-decreasing order when walking keys in order.
    pub(super) fn list(
        &self,
        entry: &BucketEntry,
        prefix: &str,
        token: Option<String>,
        max_keys: i32,
    ) -> Result<ListPage> {
        self.with_bucket(entry, |bucket| {
            let mut page = ListPage::default();
            let mut last_entry: Option<String> = None;
            let mut count = 0usize;
            for (key, object) in bucket.objects.range(prefix.to_string()..) {
                if !key.starts_with(prefix) {
                    break;
                }
                let rest = &key[prefix.len()..];
                let (entry_name, is_prefix) = match rest.find('/') {
                    Some(slash) => (format!("{}{}", prefix, &rest[..=slash]), true),
                    None => (key.clone(), false),
                };
                if token
                    .as_ref()
                    .is_some_and(|token| entry_name.as_str() <= token.as_str())
                {
                    continue;
                }
                if last_entry.as_deref() == Some(entry_name.as_str()) {
                    continue;
                }
                if count == max_keys as usize {
                    page.next_token = last_entry.clone();
                    break;
                }
                if is_prefix {
                    page.prefixes.push(entry_name.clone());
                } else {
                    page.objects.push(summary(key, object));
                }
                last_entry = Some(entry_name);
                count += 1;
            }
            Ok(page)
        })
    }

    pub(super) fn list_recursive(
        &self,
        entry: &BucketEntry,
        prefix: &str,
    ) -> Result<Vec<ObjectSummary>> {
        self.with_bucket(entry, |bucket| {
            Ok(bucket
                .objects
                .range(prefix.to_string()..)
                .take_while(|(key, _)| key.starts_with(prefix))
                .map(|(key, object)| summary(key, object))
                .collect())
        })
    }

    pub(super) fn head(&self, entry: &BucketEntry, key: &str) -> Result<ObjectDetails> {
        self.with_bucket(entry, |bucket| {
            let object = bucket
                .objects
                .get(key)
                .ok_or_else(|| anyhow!("Head s3://{}/{}: not found (404)", entry.name, key))?;
            Ok(ObjectDetails {
                content_type: Some(guess_content_type(key).to_string()),
                content_length: Some(object.data.len() as u64),
                last_modified: Some(object.last_modified),
                etag: Some(format!("\"{:032x}\"", object.data.len() * 7919)),
                storage_class: Some(object.storage_class.clone()),
                server_side_encryption: Some("AES256".to_string()),
                metadata: BTreeMap::from([("uploaded-by".to_string(), "demo".to_string())]),
                ..Default::default()
            })
        })
    }

    pub(super) fn read(
        &self,
        entry: &BucketEntry,
        key: &str,
        max_bytes: Option<u64>,
    ) -> Result<Vec<u8>> {
        self.with_bucket(entry, |bucket| {
            let object = bucket
                .objects
                .get(key)
                .ok_or_else(|| anyhow!("Get s3://{}/{}: NoSuchKey", entry.name, key))?;
            if matches!(object.storage_class.as_str(), "GLACIER" | "DEEP_ARCHIVE") {
                return Err(anyhow!(
                    "Get s3://{}/{}: InvalidObjectState: The operation is not valid for the object's storage class",
                    entry.name,
                    key
                ));
            }
            let len = max_bytes
                .map(|max| (max as usize).min(object.data.len()))
                .unwrap_or(object.data.len());
            Ok(object.data[..len].to_vec())
        })
    }

    pub(super) fn put(&self, entry: &BucketEntry, key: &str, data: Vec<u8>) -> Result<()> {
        self.with_bucket(entry, |bucket| {
            bucket.objects.insert(
                key.to_string(),
                MockObject {
                    data,
                    last_modified: Utc::now(),
                    storage_class: "STANDARD".to_string(),
                },
            );
            Ok(())
        })
    }

    pub(super) fn delete(&self, entry: &BucketEntry, keys: &[String]) -> Result<()> {
        self.with_bucket(entry, |bucket| {
            for key in keys {
                bucket.objects.remove(key);
            }
            Ok(())
        })
    }

    pub(super) fn copy(
        &self,
        source: &BucketEntry,
        source_key: &str,
        destination: &BucketEntry,
        destination_key: &str,
    ) -> Result<()> {
        let data = self.read(source, source_key, None)?;
        self.put(destination, destination_key, data)
    }
}

fn summary(key: &str, object: &MockObject) -> ObjectSummary {
    ObjectSummary {
        key: key.to_string(),
        size: object.data.len() as u64,
        last_modified: Some(object.last_modified),
        storage_class: Some(object.storage_class.clone()),
        etag: None,
    }
}

fn guess_content_type(key: &str) -> &'static str {
    match key.rsplit('.').next().unwrap_or("") {
        "csv" => "text/csv",
        "json" => "application/json",
        "md" => "text/markdown",
        "yaml" | "yml" => "application/yaml",
        "log" | "txt" => "text/plain",
        "parquet" => "application/vnd.apache.parquet",
        _ => "binary/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> BucketEntry {
        BucketEntry::new(name)
    }

    #[test]
    fn list_groups_by_delimiter_and_paginates() {
        let mut store = MockStore::empty();
        store.add_bucket("b", None, true);
        for i in 0..5 {
            store.add_object("b", &format!("dir{}/file", i), "x");
        }
        store.add_object("b", "top.txt", "x");

        let first = store.list(&entry("b"), "", None, 3).unwrap();
        assert_eq!(first.prefixes, vec!["dir0/", "dir1/", "dir2/"]);
        assert!(first.objects.is_empty());
        let token = first.next_token.clone().expect("truncated");

        let second = store.list(&entry("b"), "", Some(token), 3).unwrap();
        assert_eq!(second.prefixes, vec!["dir3/", "dir4/"]);
        assert_eq!(second.objects.len(), 1);
        assert_eq!(second.objects[0].key, "top.txt");
        assert!(second.next_token.is_none());
    }

    #[test]
    fn folder_marker_inside_prefix_is_listed_as_an_object_of_that_prefix() {
        let mut store = MockStore::empty();
        store.add_bucket("b", None, true);
        store.add_object("b", "logs/empty/", Vec::new());

        let top = store.list(&entry("b"), "logs/", None, 1000).unwrap();
        assert_eq!(top.prefixes, vec!["logs/empty/"]);

        let inner = store.list(&entry("b"), "logs/empty/", None, 1000).unwrap();
        assert!(inner.prefixes.is_empty());
        assert_eq!(inner.objects[0].key, "logs/empty/");
    }

    #[test]
    fn required_profile_is_enforced() {
        let mut store = MockStore::empty();
        store.add_bucket("secret", Some("partner"), false);
        assert!(store.list(&entry("secret"), "", None, 10).is_err());
        let with_profile = entry("secret").with_profile(Some("partner".to_string()));
        assert!(store.list(&with_profile, "", None, 10).is_ok());
    }
}
