mod aws;
pub mod mock;
pub mod navigation;
pub mod types;

pub use navigation::NavigationStack;
pub use types::*;

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

/// Maximum keys requested per listing page (the S3 maximum).
pub const LIST_PAGE_SIZE: i32 = 1000;

/// Thin, cloneable handle over either the real AWS backend or the in-memory
/// mock used by `--demo` and tests.
#[derive(Clone)]
pub struct S3Client {
    backend: Arc<Backend>,
}

enum Backend {
    Aws(aws::AwsBackend),
    Mock(mock::MockStore),
}

impl S3Client {
    pub fn new(default_profile: Option<String>, default_region: Option<String>) -> Self {
        Self {
            backend: Arc::new(Backend::Aws(aws::AwsBackend::new(
                default_profile,
                default_region,
            ))),
        }
    }

    pub fn mock() -> Self {
        Self {
            backend: Arc::new(Backend::Mock(mock::MockStore::demo())),
        }
    }

    pub fn mock_with(store: mock::MockStore) -> Self {
        Self {
            backend: Arc::new(Backend::Mock(store)),
        }
    }

    pub fn is_mock(&self) -> bool {
        matches!(*self.backend, Backend::Mock(_))
    }

    /// Profile used for a bucket entry (its own, else the default).
    pub fn effective_profile(&self, bucket: &BucketEntry) -> Option<String> {
        match &*self.backend {
            Backend::Aws(aws) => aws.effective_profile(bucket),
            Backend::Mock(mock) => mock.effective_profile(bucket),
        }
    }

    /// Region hopper resolved for the bucket, once it has talked to it.
    pub fn known_region(&self, bucket: &BucketEntry) -> Option<String> {
        match &*self.backend {
            Backend::Aws(aws) => aws.known_region(bucket),
            Backend::Mock(_) => bucket.region.clone().or(Some("us-east-1".to_string())),
        }
    }

    /// Buckets returned by ListBuckets for `profile` (the default if `None`).
    pub async fn discover_buckets(&self, profile: Option<&str>) -> Result<Vec<String>> {
        match &*self.backend {
            Backend::Aws(aws) => aws.discover_buckets(profile).await,
            Backend::Mock(mock) => mock.discover_buckets(profile),
        }
    }

    pub async fn list(
        &self,
        bucket: &BucketEntry,
        prefix: &str,
        continuation_token: Option<String>,
    ) -> Result<ListPage> {
        match &*self.backend {
            Backend::Aws(aws) => aws.list(bucket, prefix, continuation_token).await,
            Backend::Mock(mock) => mock.list(bucket, prefix, continuation_token, LIST_PAGE_SIZE),
        }
    }

    /// Every object under `prefix`, without a delimiter.
    pub async fn list_recursive(
        &self,
        bucket: &BucketEntry,
        prefix: &str,
    ) -> Result<Vec<ObjectSummary>> {
        match &*self.backend {
            Backend::Aws(aws) => aws.list_recursive(bucket, prefix).await,
            Backend::Mock(mock) => mock.list_recursive(bucket, prefix),
        }
    }

    pub async fn head(&self, bucket: &BucketEntry, key: &str) -> Result<ObjectDetails> {
        match &*self.backend {
            Backend::Aws(aws) => aws.head(bucket, key).await,
            Backend::Mock(mock) => mock.head(bucket, key),
        }
    }

    /// Reads up to `max_bytes` from the start of the object.
    pub async fn read_prefix_bytes(
        &self,
        bucket: &BucketEntry,
        key: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>> {
        match &*self.backend {
            Backend::Aws(aws) => aws.read_range(bucket, key, max_bytes).await,
            Backend::Mock(mock) => mock.read(bucket, key, Some(max_bytes)),
        }
    }

    pub async fn read_all(&self, bucket: &BucketEntry, key: &str) -> Result<Vec<u8>> {
        match &*self.backend {
            Backend::Aws(aws) => aws.read_all(bucket, key).await,
            Backend::Mock(mock) => mock.read(bucket, key, None),
        }
    }

    pub async fn download_to(&self, bucket: &BucketEntry, key: &str, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        match &*self.backend {
            Backend::Aws(aws) => aws.download_to(bucket, key, path).await,
            Backend::Mock(mock) => {
                let bytes = mock.read(bucket, key, None)?;
                tokio::fs::write(path, bytes).await?;
                Ok(())
            }
        }
    }

    pub async fn upload_file(&self, bucket: &BucketEntry, key: &str, path: &Path) -> Result<()> {
        match &*self.backend {
            Backend::Aws(aws) => aws.upload_file(bucket, key, path).await,
            Backend::Mock(mock) => {
                let bytes = tokio::fs::read(path).await?;
                mock.put(bucket, key, bytes)
            }
        }
    }

    pub async fn put_bytes(&self, bucket: &BucketEntry, key: &str, bytes: Vec<u8>) -> Result<()> {
        match &*self.backend {
            Backend::Aws(aws) => aws.put_bytes(bucket, key, bytes).await,
            Backend::Mock(mock) => mock.put(bucket, key, bytes),
        }
    }

    pub async fn delete_keys(&self, bucket: &BucketEntry, keys: Vec<String>) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        match &*self.backend {
            Backend::Aws(aws) => aws.delete_keys(bucket, keys).await,
            Backend::Mock(mock) => mock.delete(bucket, &keys),
        }
    }

    /// Server-side copy. The destination bucket's credentials are used, so
    /// they must be able to read the source.
    pub async fn copy(
        &self,
        source: &BucketEntry,
        source_key: &str,
        destination: &BucketEntry,
        destination_key: &str,
    ) -> Result<()> {
        match &*self.backend {
            Backend::Aws(aws) => {
                aws.copy(source, source_key, destination, destination_key)
                    .await
            }
            Backend::Mock(mock) => mock.copy(source, source_key, destination, destination_key),
        }
    }
}
