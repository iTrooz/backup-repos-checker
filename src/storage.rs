use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::stream::BoxStream;
use futures_util::{StreamExt, stream};
use s3::{
    Bucket, Region,
    command::Command,
    creds::Credentials,
    request::{Request, tokio_backend::ReqwestRequest},
};

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
    bucket: Box<Bucket>,
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
        let credentials = Credentials::new(
            Some(&access_key),
            Some(&secret_key),
            None,
            session_token.as_deref(),
            None,
        )?;
        Self::connect(bucket, &config.endpoint, &config.region, credentials)
    }

    /// Use path-style addressing for both AWS and compatible endpoints.
    fn connect(
        bucket: &str,
        endpoint: &str,
        region: &str,
        credentials: Credentials,
    ) -> Result<Self> {
        let region = Region::Custom {
            region: region.to_owned(),
            endpoint: endpoint.trim_end_matches('/').to_owned(),
        };
        let mut bucket =
            Bucket::new(bucket, region, credentials).context("cannot build S3 client")?;
        bucket.set_path_style();
        Ok(Self { bucket })
    }
}

#[async_trait]
impl Storage for S3Storage {
    async fn read_prefix(&self, path: &str, length: usize) -> Result<Vec<u8>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        // The client's URL parser normalizes dot segments rather than requesting them verbatim.
        anyhow::ensure!(
            !path.split('/').any(|segment| matches!(segment, "." | "..")),
            "cannot read S3 key with a dot path segment: {path:?}"
        );
        // rust-s3 consumes the first slash; add it separately from the key.
        let path = format!("/{path}");
        let request = ReqwestRequest::new(
            &self.bucket,
            &path,
            Command::GetObjectRange {
                start: 0,
                end: Some(length as u64 - 1),
            },
        )
        .await
        .context("cannot prepare S3 GetObject")?;
        let mut response = request
            .response_data_to_stream()
            .await
            .context("S3 GetObject failed")?;
        // A range starting at byte zero is unsatisfiable only for an empty object.
        if response.status_code == 416 {
            return Ok(Vec::new());
        }
        anyhow::ensure!(
            (200..300).contains(&response.status_code),
            "S3 GetObject failed: HTTP {}",
            response.status_code
        );
        let mut bytes = Vec::with_capacity(length);
        while bytes.len() < length {
            let Some(chunk) = response.bytes.next().await else {
                break;
            };
            let chunk = chunk.context("cannot read S3 response body")?;
            bytes.extend_from_slice(&chunk[..chunk.len().min(length - bytes.len())]);
        }
        Ok(bytes)
    }

    async fn list(&self) -> Result<BoxStream<'_, Result<FileEntry>>> {
        // Retain raw S3 keys: normalizing paths would lose directory markers.
        let pages = stream::try_unfold(Some(None), move |state| async move {
            let Some(token) = state else {
                return Ok::<_, anyhow::Error>(None);
            };
            let (page, status) = self
                .bucket
                .list_page(String::new(), None, token.clone(), None, None)
                .await
                .context("S3 ListObjectsV2 failed")?;
            anyhow::ensure!(status == 200, "S3 ListObjectsV2 failed: HTTP {status}");
            let next = if page.is_truncated {
                let next = page
                    .next_continuation_token
                    .filter(|next| !next.is_empty())
                    .context("truncated S3 listing omitted continuation token")?;
                anyhow::ensure!(
                    Some(next.as_str()) != token.as_deref(),
                    "S3 listing repeated continuation token"
                );
                Some(Some(next))
            } else {
                None
            };
            let entries = page
                .contents
                .into_iter()
                .map(|object| {
                    let modified = DateTime::parse_from_rfc3339(&object.last_modified)
                        .context("S3 object has invalid LastModified")?
                        .with_timezone(&Utc);
                    Ok(FileEntry {
                        path: object.key,
                        modified,
                        size: object.size,
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
    use futures_util::TryStreamExt;
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
        let credentials = Credentials::new(Some("test"), Some("test"), None, None, None).unwrap();
        let storage =
            S3Storage::connect("test-bucket", &endpoint, "us-east-1", credentials).unwrap();
        (storage, server)
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

    /// Listing keys must round-trip through the signed GET request without normalization.
    #[tokio::test]
    async fn special_object_keys_round_trip() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let page = b"<ListBucketResult><Name>test-bucket</Name><IsTruncated>false</IsTruncated><Contents><Key>/repo//file +%?#&amp;.txt</Key><LastModified>2026-01-01T00:00:00Z</LastModified><Size>4</Size></Contents></ListBucketResult>";
            let (storage, server) =
                mock_responses(vec![response("200 OK", page), response("206 Partial Content", b"data")])
                    .await;
            let entries: Vec<_> = storage.list().await.unwrap().try_collect().await.unwrap();
            assert_eq!(entries[0].path, "/repo//file +%?#&.txt");
            assert_eq!(storage.read_prefix(&entries[0].path, 4).await.unwrap(), b"data");
            let requests = server.await.unwrap();
            assert_eq!(
                requests[1]
                    .split_whitespace()
                    .nth(1)
                    .and_then(|target| target.split('?').next()),
                Some("/test-bucket//repo//file%20%2B%25%3F%23%26.txt")
            );
        })
        .await
        .unwrap();
    }

    /// Do not check the wrong object when the HTTP client would rewrite the key.
    #[tokio::test]
    async fn dot_segments_are_rejected_before_sending() {
        let (storage, server) = mock_responses(Vec::new()).await;
        assert!(server.await.unwrap().is_empty());
        for key in ["repo/./file", "repo/nested/../file"] {
            let error = storage.read_prefix(key, 4).await.unwrap_err();
            assert!(error.to_string().contains("dot path segment"));
        }
    }

    #[tokio::test]
    async fn paginates_and_preserves_directory_markers() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (storage, server) = mock_s3(vec![
                "<ListBucketResult><Name>test-bucket</Name><IsTruncated>true</IsTruncated><NextContinuationToken>page-two</NextContinuationToken><Contents><Key>repo/</Key><LastModified>2026-01-01T00:00:00Z</LastModified><Size>0</Size></Contents></ListBucketResult>".into(),
                "<ListBucketResult><Name>test-bucket</Name><IsTruncated>false</IsTruncated><Contents><Key>repo/deep/file</Key><LastModified>2026-01-02T00:00:00Z</LastModified><Size>42</Size></Contents></ListBucketResult>".into(),
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
                "<ListBucketResult><Name>test-bucket</Name><IsTruncated>true</IsTruncated></ListBucketResult>".into(),
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

    /// A broken endpoint must not make the checker loop over the same listing page.
    #[tokio::test]
    async fn repeated_continuation_token_is_rejected() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let page = "<ListBucketResult><Name>test-bucket</Name><IsTruncated>true</IsTruncated><NextContinuationToken>same</NextContinuationToken></ListBucketResult>";
            let (storage, server) = mock_s3(vec![page.to_owned(), page.to_owned()]).await;
            let entries: Result<Vec<_>> = storage.list().await.unwrap().try_collect().await;
            assert!(
                entries
                    .unwrap_err()
                    .to_string()
                    .contains("repeated continuation token")
            );
            assert_eq!(server.await.unwrap().len(), 2);
        })
        .await
        .unwrap();
    }
}
