use anyhow::{Context, Result};
use async_trait::async_trait;
use aws_sdk_s3::{
    Client,
    config::{BehaviorVersion, Credentials, Region},
};
use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use futures::{StreamExt, stream};
use tokio::io::AsyncReadExt;

use crate::config::{ProviderConfig, S3Config};

impl ProviderConfig {
    /// Create a bucket-scoped backend, loading only this provider's credentials.
    pub fn storage(&self, bucket: &str) -> Result<Box<dyn Storage>> {
        match self {
            Self::S3(config) => Ok(Box::new(S3Storage::new(config, bucket)?)),
        }
    }
}

#[derive(Debug)]
pub struct FileEntry {
    pub path: String,
    pub modified: DateTime<Utc>,
    pub size: u64,
}

/// Implementations recursively stream all files, including directory markers.
/// Errors must propagate: an incomplete listing cannot establish missing repositories.
#[async_trait]
pub trait Storage: Send + Sync {
    async fn list(&self) -> Result<BoxStream<'_, Result<FileEntry>>>;
    /// Return at most `length` leading bytes; empty files return an empty vector.
    async fn read_prefix(&self, path: &str, length: usize) -> Result<Vec<u8>>;
}

pub struct S3Storage {
    client: Client,
    bucket: String,
}

impl S3Storage {
    /// Build an S3 client using this provider's endpoint and credential environment.
    pub fn new(config: &S3Config, bucket: &str) -> Result<Self> {
        let access_key = std::env::var(&config.access_key_id_env)
            .with_context(|| format!("{} must be set", config.access_key_id_env))?;
        let secret_key = std::env::var(&config.secret_access_key_env)
            .with_context(|| format!("{} must be set", config.secret_access_key_env))?;
        anyhow::ensure!(
            !access_key.is_empty() && !secret_key.is_empty(),
            "S3 credentials cannot be empty"
        );
        let session_token = config
            .session_token_env
            .as_ref()
            .map(|name| std::env::var(name).with_context(|| format!("{name} must be set")))
            .transpose()?
            .filter(|token| !token.is_empty());
        let credentials =
            Credentials::new(access_key, secret_key, session_token, None, "environment");
        let sdk_config = aws_sdk_s3::config::Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()))
            .endpoint_url(&config.endpoint)
            .credentials_provider(credentials)
            .force_path_style(true)
            .build();
        Ok(Self {
            client: Client::from_conf(sdk_config),
            bucket: bucket.to_owned(),
        })
    }
}

#[async_trait]
impl Storage for S3Storage {
    async fn read_prefix(&self, path: &str, length: usize) -> Result<Vec<u8>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(path)
            .range(format!("bytes=0-{}", length - 1))
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            // A range starting at byte zero is unsatisfiable only for an empty object.
            Err(error)
                if error
                    .raw_response()
                    .is_some_and(|response| response.status().as_u16() == 416) =>
            {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error).context("S3 GetObject failed"),
        };
        let mut bytes = Vec::with_capacity(length);
        response
            .body
            .into_async_read()
            .take(length as u64)
            .read_to_end(&mut bytes)
            .await
            .context("cannot read S3 response body")?;
        Ok(bytes)
    }

    async fn list(&self) -> Result<BoxStream<'_, Result<FileEntry>>> {
        // Retain raw S3 keys: normalizing paths would lose directory markers.
        let pages = stream::try_unfold(Some(None), move |state| async move {
            let Some(token) = state else {
                return Ok::<_, anyhow::Error>(None);
            };
            let page = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .set_continuation_token(token.clone())
                .send()
                .await
                .context("S3 ListObjectsV2 failed")?;
            let next = if page.is_truncated() == Some(true) {
                let next = page
                    .next_continuation_token()
                    .filter(|next| !next.is_empty())
                    .context("truncated S3 listing omitted continuation token")?;
                anyhow::ensure!(
                    Some(next) != token.as_deref(),
                    "S3 listing repeated continuation token"
                );
                Some(Some(next.to_owned()))
            } else {
                None
            };
            let entries = page
                .contents()
                .iter()
                .map(|object| {
                    let path = object.key().context("S3 object omitted key")?.to_owned();
                    let modified = object
                        .last_modified()
                        .context("S3 object omitted LastModified")?;
                    let modified =
                        DateTime::from_timestamp(modified.secs(), modified.subsec_nanos())
                            .context("S3 object has invalid LastModified")?;
                    let size = object.size().context("S3 object omitted Size")?;
                    anyhow::ensure!(size >= 0, "S3 object has negative Size");
                    Ok(FileEntry {
                        path,
                        modified,
                        size: size as u64,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Some((entries, next)))
        });
        Ok(Box::pin(pages.flat_map(|page| {
            stream::iter(match page {
                Ok(entries) => entries.into_iter().map(Ok).collect(),
                Err(error) => vec![Err(error)],
            })
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn mock_s3(pages: Vec<String>) -> (S3Storage, tokio::task::JoinHandle<Vec<String>>) {
        mock_responses(
            pages
                .into_iter()
                .map(|page| response("200 OK", page.as_bytes()))
                .collect(),
        )
        .await
    }

    fn response(status: &str, body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len(),
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    async fn mock_responses(
        responses: Vec<Vec<u8>>,
    ) -> (S3Storage, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 4096];
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                requests.push(String::from_utf8(request).unwrap());
                socket.write_all(&response).await.unwrap();
            }
            requests
        });
        let config = aws_sdk_s3::config::Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .endpoint_url(endpoint)
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new("test", "test", None, None, "test"))
            .force_path_style(true)
            .build();
        (
            S3Storage {
                client: Client::from_conf(config),
                bucket: "test-bucket".into(),
            },
            server,
        )
    }

    #[tokio::test]
    async fn range_reads_preserve_binary_bytes_and_bound_ignored_ranges() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let binary = [0xff, 0, b'\n', 0x80];
            let (storage, server) = mock_responses(vec![
                response("206 Partial Content", &binary),
                response("200 OK", &[42; 128]),
            ])
            .await;
            assert_eq!(
                storage.read_prefix("repo/deep/file", 64).await.unwrap(),
                binary
            );
            assert_eq!(
                storage.read_prefix("repo/other", 64).await.unwrap(),
                vec![42; 64]
            );
            let requests = server.await.unwrap();
            assert!(requests[0].contains("/test-bucket/repo/deep/file"));
            assert!(
                requests
                    .iter()
                    .all(|request| request.to_ascii_lowercase().contains("range: bytes=0-63"))
            );
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn empty_objects_are_empty_but_access_errors_propagate() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (storage, server) = mock_responses(vec![
                response(
                    "416 Range Not Satisfiable",
                    b"<Error><Code>InvalidRange</Code></Error>",
                ),
                response("403 Forbidden", b"<Error><Code>AccessDenied</Code></Error>"),
            ])
            .await;
            assert!(storage.read_prefix("empty", 64).await.unwrap().is_empty());
            assert!(storage.read_prefix("private", 64).await.is_err());
            assert_eq!(server.await.unwrap().len(), 2);
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn paginates_and_preserves_directory_markers() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (storage, server) = mock_s3(vec![
                "<ListBucketResult><IsTruncated>true</IsTruncated><NextContinuationToken>page-two</NextContinuationToken><Contents><Key>repo/</Key><LastModified>2026-01-01T00:00:00Z</LastModified><Size>0</Size></Contents></ListBucketResult>".into(),
                "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>repo/deep/file</Key><LastModified>2026-01-02T00:00:00Z</LastModified><Size>42</Size></Contents></ListBucketResult>".into(),
            ]).await;
            let entries: Vec<_> = storage.list().await.unwrap().try_collect().await.unwrap();
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].path, "repo/");
            assert_eq!(entries[1].path, "repo/deep/file");
            assert_eq!(entries[0].size, 0);
            assert_eq!(entries[1].size, 42);
            assert!(entries[1].modified > entries[0].modified);
            let requests = server.await.unwrap();
            let target = requests[0].split_whitespace().nth(1).unwrap();
            assert_eq!(target.split('?').next().unwrap().trim_end_matches('/'), "/test-bucket");
            assert!(target.contains("list-type=2"));
            assert!(requests[1].contains("continuation-token=page-two"));
            assert!(requests.iter().all(|request| request.to_lowercase().contains("authorization: aws4-hmac-sha256")));
        }).await.unwrap();
    }

    #[tokio::test]
    async fn truncated_listing_requires_a_continuation_token() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (storage, server) = mock_s3(vec![
                "<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>".into(),
            ])
            .await;
            let entries: Result<Vec<_>> = storage.list().await.unwrap().try_collect().await;
            assert!(
                entries
                    .unwrap_err()
                    .to_string()
                    .contains("continuation token")
            );
            server.await.unwrap();
        })
        .await
        .unwrap();
    }
}
