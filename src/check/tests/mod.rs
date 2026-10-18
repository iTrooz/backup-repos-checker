//! Shared scan fixtures; behavior tests are grouped by responsibility.

/// Recursive timestamp evaluation and freshness rendering.
mod age;

/// Content validation and failure aggregation.
mod content;
/// Folder discovery and recursive listing.
mod folders;
/// Independent run-wide check selection.
mod selection;

use super::*;
use crate::storage::FileEntry;
use async_trait::async_trait;
use futures::{stream, stream::BoxStream};

/// In-memory files with observable reads and optional access failures.
struct TestStorage {
    files: BTreeMap<String, Vec<u8>>,
    reads: std::sync::atomic::AtomicUsize,
    fail_reads: bool,
}

#[async_trait]
impl Storage for TestStorage {
    /// Return a recursive inventory without reading file contents.
    async fn list(&self) -> Result<BoxStream<'_, Result<FileEntry>>> {
        Ok(Box::pin(stream::iter(self.files.keys().map(|path| {
            Ok(FileEntry {
                path: path.clone(),
                modified: Utc::now(),
            })
        }))))
    }

    /// Track prefix reads and optionally simulate permission errors.
    async fn read_prefix(&self, path: &str, length: usize) -> Result<Vec<u8>> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail_reads {
            anyhow::bail!("access denied");
        }
        assert_eq!(length, super::content::PREFIX_BYTES);
        Ok(self.files[path].iter().copied().take(length).collect())
    }
}
