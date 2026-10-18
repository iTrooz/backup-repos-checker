//! Recursive freshness and timestamp-dependent tree rendering.

use super::*;

/// Timestamp fixtures that must never download object contents.
struct MetadataStorage(Vec<(&'static str, DateTime<Utc>)>);

#[async_trait]
impl Storage for MetadataStorage {
    /// Fail immediately if a freshness-only test downloads a file.
    async fn read_prefix(&self, _path: &str, _length: usize) -> Result<Vec<u8>> {
        panic!("freshness-only checks must not download objects");
    }

    /// Stream configured object timestamps for deterministic age checks.
    async fn list(&self) -> Result<BoxStream<'_, Result<FileEntry>>> {
        Ok(Box::pin(stream::iter(self.0.iter().map(
            |(path, modified)| {
                Ok(FileEntry {
                    path: (*path).to_owned(),
                    modified: *modified,
                })
            },
        ))))
    }
}

/// Adapt metadata fixtures to the full default selection.
async fn inspect_prefixes(storage: &dyn Storage, prefixes: &[String]) -> Result<Folder> {
    let repositories: Vec<_> = prefixes
        .iter()
        .map(|prefix| RepositoryLocation {
            prefix,
            content_check: None,
        })
        .collect();
    inspect(storage, &repositories, Selection::default()).await
}

/// Recursive timestamps and directory markers determine the expected tree colors.
#[tokio::test]
async fn recursive_freshness_and_tree_colors() {
    let now = Utc::now();
    let storage = MetadataStorage(vec![
        ("backups/fresh/deep/file", now - TimeDelta::days(7)),
        ("backups/fresh/old", now - TimeDelta::days(20)),
        ("backups/stale/file", now - TimeDelta::days(8)),
        ("backups/empty/", now),
        ("untracked/child/file", now),
    ]);
    let prefixes =
        ["backups/fresh", "backups/stale", "backups/empty", "missing"].map(str::to_owned);
    let tree = inspect_prefixes(&storage, &prefixes).await.unwrap();
    let rendered = tree.render("s3://test", now, 7, true, Selection::default());
    for expected in [
        "\x1b[32mfresh",
        "\x1b[33mstale",
        "\x1b[33mempty  [no files]",
        "\x1b[31;4mmissing  [not found]",
        "\x1b[31muntracked",
    ] {
        assert!(rendered.contains(expected), "{expected}");
    }
    assert!(!rendered.contains("deep"));
    assert!(!rendered.contains("\x1b[31mbackups"));
    assert!(tree.failed(Selection::default()));
    assert!(
        !tree
            .render("s3://test", now, 7, false, Selection::default())
            .contains('\x1b')
    );
    let issues = tree.issues("s3://test", now, 7, Selection::default());
    assert_eq!(issues.len(), 5);
    assert!(
        issues
            .iter()
            .any(|issue| issue == "[MISSING] s3://test/missing")
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue == "[UNCONFIGURED] s3://test/untracked/child")
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue.starts_with("[STALE] s3://test/backups/empty (no files)"))
    );
    assert!(
        !issues
            .iter()
            .any(|issue| issue.contains("fresh") || issue.contains("deep"))
    );
}
