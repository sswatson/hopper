use super::LIST_PAGE_SIZE;
use super::types::{BucketEntry, ListPage, ObjectDetails, ObjectSummary};
use anyhow::{Result, anyhow};
use aws_config::{BehaviorVersion, SdkConfig};
use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

const FALLBACK_REGION: &str = "us-east-1";
const DELETE_BATCH_SIZE: usize = 1000;

type ProfileKey = Option<String>;

/// Real S3 access. SDK configs are loaded once per profile and clients are
/// cached per (profile, region); each bucket's region is discovered with
/// HeadBucket the first time it is used, so buckets in different regions
/// and accounts can sit side by side.
pub(super) struct AwsBackend {
    default_profile: ProfileKey,
    default_region: Option<String>,
    sdk_configs: tokio::sync::Mutex<HashMap<ProfileKey, SdkConfig>>,
    clients: Mutex<HashMap<(ProfileKey, String), Client>>,
    regions: Mutex<HashMap<(ProfileKey, String), String>>,
}

impl AwsBackend {
    pub(super) fn new(default_profile: Option<String>, default_region: Option<String>) -> Self {
        Self {
            default_profile,
            default_region,
            sdk_configs: tokio::sync::Mutex::new(HashMap::new()),
            clients: Mutex::new(HashMap::new()),
            regions: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn effective_profile(&self, bucket: &BucketEntry) -> ProfileKey {
        bucket
            .profile
            .clone()
            .or_else(|| self.default_profile.clone())
    }

    pub(super) fn known_region(&self, bucket: &BucketEntry) -> Option<String> {
        if let Some(region) = &bucket.region {
            return Some(region.clone());
        }
        let key = (self.effective_profile(bucket), bucket.name.clone());
        self.regions.lock().ok()?.get(&key).cloned()
    }

    async fn sdk_config(&self, profile: &ProfileKey) -> SdkConfig {
        let mut configs = self.sdk_configs.lock().await;
        if let Some(config) = configs.get(profile) {
            return config.clone();
        }
        let mut loader = aws_config::defaults(BehaviorVersion::latest());
        if let Some(profile) = profile {
            loader = loader.profile_name(profile);
        }
        if let Some(region) = &self.default_region {
            loader = loader.region(Region::new(region.clone()));
        }
        let config = loader.load().await;
        configs.insert(profile.clone(), config.clone());
        config
    }

    fn client_in_region(&self, profile: &ProfileKey, config: &SdkConfig, region: &str) -> Client {
        let key = (profile.clone(), region.to_string());
        let mut clients = self.clients.lock().expect("client cache poisoned");
        clients
            .entry(key)
            .or_insert_with(|| {
                let s3_config = aws_sdk_s3::config::Builder::from(config)
                    .region(Region::new(region.to_string()))
                    .build();
                Client::from_conf(s3_config)
            })
            .clone()
    }

    fn base_region(config: &SdkConfig) -> String {
        config
            .region()
            .map(|region| region.to_string())
            .unwrap_or_else(|| FALLBACK_REGION.to_string())
    }

    async fn client_for(&self, bucket: &BucketEntry) -> Client {
        let profile = self.effective_profile(bucket);
        let config = self.sdk_config(&profile).await;
        let region = match self.known_region(bucket) {
            Some(region) => region,
            None => {
                let region = self.resolve_region(&profile, &config, &bucket.name).await;
                if let Ok(mut regions) = self.regions.lock() {
                    regions.insert((profile.clone(), bucket.name.clone()), region.clone());
                }
                region
            }
        };
        self.client_in_region(&profile, &config, &region)
    }

    /// S3 reports a bucket's region in the `x-amz-bucket-region` header of a
    /// HeadBucket response, even when the request is denied or sent to the
    /// wrong region, so this works for cross-account buckets too.
    async fn resolve_region(
        &self,
        profile: &ProfileKey,
        config: &SdkConfig,
        bucket: &str,
    ) -> String {
        let base = Self::base_region(config);
        let client = self.client_in_region(profile, config, &base);
        match client.head_bucket().bucket(bucket).send().await {
            Ok(output) => output.bucket_region().map(str::to_string).unwrap_or(base),
            Err(err) => err
                .raw_response()
                .and_then(|response| response.headers().get("x-amz-bucket-region"))
                .map(str::to_string)
                .unwrap_or(base),
        }
    }

    fn error<E>(
        &self,
        bucket: &BucketEntry,
        context: String,
        err: SdkError<E, HttpResponse>,
    ) -> anyhow::Error
    where
        E: ProvideErrorMetadata + std::error::Error + 'static,
    {
        let profile = self.effective_profile(bucket);
        anyhow!(
            "{}: {}",
            context,
            describe_sdk_error(&err, profile.as_deref())
        )
    }

    pub(super) async fn discover_buckets(&self, profile: Option<&str>) -> Result<Vec<String>> {
        let profile: ProfileKey = profile
            .map(str::to_string)
            .or_else(|| self.default_profile.clone());
        let config = self.sdk_config(&profile).await;
        let client = self.client_in_region(&profile, &config, &Self::base_region(&config));

        let mut names = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut request = client.list_buckets();
            if let Some(token) = token.take() {
                request = request.continuation_token(token);
            }
            let output = request.send().await.map_err(|err| {
                anyhow!(
                    "ListBuckets ({}): {}",
                    profile.as_deref().unwrap_or("default"),
                    describe_sdk_error(&err, profile.as_deref())
                )
            })?;
            names.extend(
                output
                    .buckets()
                    .iter()
                    .filter_map(|bucket| bucket.name().map(str::to_string)),
            );
            match output.continuation_token() {
                Some(next) if !next.is_empty() => token = Some(next.to_string()),
                _ => break,
            }
        }
        Ok(names)
    }

    pub(super) async fn list(
        &self,
        bucket: &BucketEntry,
        prefix: &str,
        continuation_token: Option<String>,
    ) -> Result<ListPage> {
        let client = self.client_for(bucket).await;
        let output = client
            .list_objects_v2()
            .bucket(&bucket.name)
            .prefix(prefix)
            .delimiter("/")
            .max_keys(LIST_PAGE_SIZE)
            .set_continuation_token(continuation_token)
            .send()
            .await
            .map_err(|err| {
                self.error(bucket, format!("List s3://{}/{}", bucket.name, prefix), err)
            })?;

        Ok(ListPage {
            prefixes: output
                .common_prefixes()
                .iter()
                .filter_map(|p| p.prefix().map(str::to_string))
                .collect(),
            objects: output.contents().iter().map(object_summary).collect(),
            next_token: if output.is_truncated() == Some(true) {
                output.next_continuation_token().map(str::to_string)
            } else {
                None
            },
        })
    }

    pub(super) async fn list_recursive(
        &self,
        bucket: &BucketEntry,
        prefix: &str,
    ) -> Result<Vec<ObjectSummary>> {
        let client = self.client_for(bucket).await;
        let mut objects = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let output = client
                .list_objects_v2()
                .bucket(&bucket.name)
                .prefix(prefix)
                .max_keys(LIST_PAGE_SIZE)
                .set_continuation_token(token.take())
                .send()
                .await
                .map_err(|err| {
                    self.error(bucket, format!("List s3://{}/{}", bucket.name, prefix), err)
                })?;
            objects.extend(output.contents().iter().map(object_summary));
            match (output.is_truncated(), output.next_continuation_token()) {
                (Some(true), Some(next)) => token = Some(next.to_string()),
                _ => break,
            }
        }
        Ok(objects)
    }

    pub(super) async fn head(&self, bucket: &BucketEntry, key: &str) -> Result<ObjectDetails> {
        let client = self.client_for(bucket).await;
        let output = client
            .head_object()
            .bucket(&bucket.name)
            .key(key)
            .send()
            .await
            .map_err(|err| self.error(bucket, format!("Head s3://{}/{}", bucket.name, key), err))?;

        Ok(ObjectDetails {
            content_type: output.content_type().map(str::to_string),
            content_length: output.content_length().map(|len| len.max(0) as u64),
            content_encoding: output.content_encoding().map(str::to_string),
            cache_control: output.cache_control().map(str::to_string),
            last_modified: output.last_modified().and_then(to_chrono),
            etag: output.e_tag().map(str::to_string),
            storage_class: output
                .storage_class()
                .map(|class| class.as_str().to_string()),
            server_side_encryption: output
                .server_side_encryption()
                .map(|sse| sse.as_str().to_string()),
            version_id: output.version_id().map(str::to_string),
            metadata: output
                .metadata()
                .map(|metadata| {
                    metadata
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    async fn get_object(
        &self,
        bucket: &BucketEntry,
        key: &str,
        range: Option<String>,
    ) -> Result<ByteStream> {
        let client = self.client_for(bucket).await;
        let output = client
            .get_object()
            .bucket(&bucket.name)
            .key(key)
            .set_range(range)
            .send()
            .await
            .map_err(|err| self.error(bucket, format!("Get s3://{}/{}", bucket.name, key), err))?;
        Ok(output.body)
    }

    pub(super) async fn read_range(
        &self,
        bucket: &BucketEntry,
        key: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>> {
        let range = format!("bytes=0-{}", max_bytes.saturating_sub(1));
        let body = self.get_object(bucket, key, Some(range)).await?;
        let bytes = body
            .collect()
            .await
            .map_err(|err| anyhow!("Read s3://{}/{}: {}", bucket.name, key, err))?;
        Ok(bytes.into_bytes().to_vec())
    }

    pub(super) async fn read_all(&self, bucket: &BucketEntry, key: &str) -> Result<Vec<u8>> {
        let body = self.get_object(bucket, key, None).await?;
        let bytes = body
            .collect()
            .await
            .map_err(|err| anyhow!("Read s3://{}/{}: {}", bucket.name, key, err))?;
        Ok(bytes.into_bytes().to_vec())
    }

    /// Streams the object to `<path>.part` and renames it into place, so an
    /// interrupted download never leaves a truncated file under the real name.
    pub(super) async fn download_to(
        &self,
        bucket: &BucketEntry,
        key: &str,
        path: &Path,
    ) -> Result<()> {
        let body = self.get_object(bucket, key, None).await?;
        let partial = path.with_extension(match path.extension() {
            Some(ext) => format!("{}.part", ext.to_string_lossy()),
            None => "part".to_string(),
        });
        let mut reader = body.into_async_read();
        let mut file = tokio::fs::File::create(&partial).await?;
        let copied = tokio::io::copy(&mut reader, &mut file).await;
        drop(file);
        if let Err(err) = copied {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(anyhow!("Download s3://{}/{}: {}", bucket.name, key, err));
        }
        tokio::fs::rename(&partial, path).await?;
        Ok(())
    }

    pub(super) async fn upload_file(
        &self,
        bucket: &BucketEntry,
        key: &str,
        path: &Path,
    ) -> Result<()> {
        let body = ByteStream::from_path(path)
            .await
            .map_err(|err| anyhow!("Read {}: {}", path.display(), err))?;
        self.put(bucket, key, body).await
    }

    pub(super) async fn put_bytes(
        &self,
        bucket: &BucketEntry,
        key: &str,
        bytes: Vec<u8>,
    ) -> Result<()> {
        self.put(bucket, key, ByteStream::from(bytes)).await
    }

    async fn put(&self, bucket: &BucketEntry, key: &str, body: ByteStream) -> Result<()> {
        let client = self.client_for(bucket).await;
        client
            .put_object()
            .bucket(&bucket.name)
            .key(key)
            .body(body)
            .send()
            .await
            .map_err(|err| self.error(bucket, format!("Put s3://{}/{}", bucket.name, key), err))?;
        Ok(())
    }

    pub(super) async fn delete_keys(&self, bucket: &BucketEntry, keys: Vec<String>) -> Result<()> {
        let client = self.client_for(bucket).await;
        for chunk in keys.chunks(DELETE_BATCH_SIZE) {
            let objects = chunk
                .iter()
                .map(|key| ObjectIdentifier::builder().key(key).build())
                .collect::<Result<Vec<_>, _>>()?;
            let delete = Delete::builder()
                .set_objects(Some(objects))
                .quiet(true)
                .build()?;
            let output = client
                .delete_objects()
                .bucket(&bucket.name)
                .delete(delete)
                .send()
                .await
                .map_err(|err| {
                    self.error(bucket, format!("Delete in s3://{}", bucket.name), err)
                })?;
            if let Some(first) = output.errors().first() {
                return Err(anyhow!(
                    "Delete s3://{}/{}: {}{} ({} failed)",
                    bucket.name,
                    first.key().unwrap_or("?"),
                    first.code().unwrap_or("error"),
                    first
                        .message()
                        .map(|message| format!(": {}", message))
                        .unwrap_or_default(),
                    output.errors().len()
                ));
            }
        }
        Ok(())
    }

    pub(super) async fn copy(
        &self,
        source: &BucketEntry,
        source_key: &str,
        destination: &BucketEntry,
        destination_key: &str,
    ) -> Result<()> {
        let client = self.client_for(destination).await;
        client
            .copy_object()
            .bucket(&destination.name)
            .key(destination_key)
            .copy_source(format!(
                "{}/{}",
                source.name,
                encode_copy_source_key(source_key)
            ))
            .send()
            .await
            .map_err(|err| {
                self.error(
                    destination,
                    format!(
                        "Copy s3://{}/{} -> s3://{}/{}",
                        source.name, source_key, destination.name, destination_key
                    ),
                    err,
                )
            })?;
        Ok(())
    }
}

fn object_summary(object: &aws_sdk_s3::types::Object) -> ObjectSummary {
    ObjectSummary {
        key: object.key().unwrap_or_default().to_string(),
        size: object.size().unwrap_or(0).max(0) as u64,
        last_modified: object.last_modified().and_then(to_chrono),
        storage_class: object
            .storage_class()
            .map(|class| class.as_str().to_string()),
        etag: object.e_tag().map(str::to_string),
    }
}

fn to_chrono(datetime: &aws_sdk_s3::primitives::DateTime) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(datetime.secs(), datetime.subsec_nanos())
}

/// CopySource must be URL-encoded; keep `/` so the key's structure survives.
fn encode_copy_source_key(key: &str) -> String {
    let mut encoded = String::with_capacity(key.len());
    for byte in key.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{:02X}", byte)),
        }
    }
    encoded
}

/// Produces a one-line error: the S3 error code and message when present,
/// a readable HTTP status for bodiless responses (HEAD), and otherwise the
/// full error chain, which is where credential problems show up.
fn describe_sdk_error<E>(err: &SdkError<E, HttpResponse>, profile: Option<&str>) -> String
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    if let Some(code) = err.code() {
        return match err.message() {
            Some(message) if !message.is_empty() => format!("{}: {}", code, message),
            _ => code.to_string(),
        };
    }

    if let Some(response) = err.raw_response() {
        return match response.status().as_u16() {
            301 => "bucket is in a different region (301)".to_string(),
            403 => "access denied (403 Forbidden)".to_string(),
            404 => "not found (404)".to_string(),
            status => format!("HTTP {}", status),
        };
    }

    let text = DisplayErrorContext(err).to_string();
    let lower = text.to_ascii_lowercase();
    if lower.contains("sso") || lower.contains("token") || lower.contains("credential") {
        let login = match profile {
            Some(profile) => format!("aws sso login --profile {}", profile),
            None => "aws sso login".to_string(),
        };
        format!("{} (credentials problem; try `{}`)", text, login)
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::encode_copy_source_key;

    #[test]
    fn copy_source_encoding_keeps_slashes_and_escapes_the_rest() {
        assert_eq!(encode_copy_source_key("a/b c+d.txt"), "a/b%20c%2Bd.txt");
        assert_eq!(encode_copy_source_key("ünï"), "%C3%BCn%C3%AF");
    }
}
