//! Exercise recursive content checks and independent check selection.

use super::*;
use crate::storage::FileEntry;
use async_trait::async_trait;
use futures::{stream, stream::BoxStream};

/// In-memory files with observable reads and optional access failures.
struct HeaderStorage {
    files: BTreeMap<String, Vec<u8>>,
    reads: std::sync::atomic::AtomicUsize,
    fail_reads: bool,
}

#[async_trait]
impl Storage for HeaderStorage {
    /// Return a complete recursive inventory without reading file contents.
    async fn list(&self) -> Result<BoxStream<'_, Result<FileEntry>>> {
        Ok(Box::pin(stream::iter(self.files.keys().map(|path| {
            Ok(FileEntry {
                path: path.clone(),
                modified: Utc::now(),
            })
        }))))
    }

    /// Track prefix reads and simulate permission errors when requested.
    async fn read_prefix(&self, path: &str, length: usize) -> Result<Vec<u8>> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail_reads {
            anyhow::bail!("access denied");
        }
        assert_eq!(length, crate::header::HEADER_BYTES);
        Ok(self.files[path].iter().copied().take(length).collect())
    }
}

/// Match recursively, ignore markers, and fail unmatched or invalid files.
#[tokio::test]
async fn header_checks_are_recursive_and_fail_closed() {
    let storage = HeaderStorage {
        files: [
            ("repo/root.gz", vec![0x1f, 0x8b, 0x08]),
            ("repo/deep/bad.gz", b"wrong".to_vec()),
            ("repo/empty.gz", vec![]),
            ("repo/unmatched.txt", b"text".to_vec()),
            ("repo/marker/", vec![]),
            ("other/file.gz", b"ignored".to_vec()),
        ]
        .into_iter()
        .map(|(path, bytes)| (path.to_owned(), bytes))
        .collect(),
        reads: 0.into(),
        fail_reads: false,
    };
    let header_check =
        toml::from_str("[[rules]]\nglob = '**/*.gz'\npattern = '^\\x1f\\x8b\\x08'").unwrap();
    let repositories = [RepositoryLocation {
        prefix: "repo",
        header_check: Some(&header_check),
    }];
    let tree = inspect(&storage, &repositories, Selection::default())
        .await
        .unwrap();
    assert!(tree.failed(Selection::default()));
    assert_eq!(storage.reads.load(std::sync::atomic::Ordering::SeqCst), 3);
    let rendered = tree.render("s3://test", Utc::now(), 7, true, Selection::default());
    assert!(rendered.contains("\x1b[31mrepo"));
    assert!(rendered.contains("[header failures: 3/4]"));
    assert!(!rendered.contains("bad.gz"));
    let issues = tree
        .issues("s3://test", Utc::now(), 7, Selection::default())
        .join("\n");
    assert!(issues.contains("[HEADER CHECK FAILED] s3://test/repo"));
    assert!(issues.contains("deep/bad.gz"));
    assert!(issues.contains("no matching header rule"));
    assert!(!issues.contains("root.gz"));
    assert!(!issues.contains("[STALE]"));
}

/// Continue checking all files while capping diagnostic samples.
#[tokio::test]
async fn checks_every_file_but_bounds_failure_details() {
    let storage = HeaderStorage {
        files: (0..27)
            .map(|i| (format!("deep/file{i}"), b"bad".to_vec()))
            .collect(),
        reads: 0.into(),
        fail_reads: false,
    };
    let header_check = toml::from_str("[[rules]]\nglob = '**'\npattern = '^GOOD'").unwrap();
    let repositories = [RepositoryLocation {
        prefix: "",
        header_check: Some(&header_check),
    }];
    let tree = inspect(&storage, &repositories, Selection::default())
        .await
        .unwrap();
    assert_eq!(storage.reads.load(std::sync::atomic::Ordering::SeqCst), 27);
    let repo = tree.repository.as_ref().unwrap();
    assert_eq!(repo.header_failures, 27);
    assert_eq!(repo.failure_samples.len(), MAX_FAILURE_SAMPLES);
    assert!(
        tree.issues("s3://test", Utc::now(), 7, Selection::default())[0]
            .contains("additional failures omitted")
    );
}

/// Distinguish failed downloads from files that fail header validation.
#[tokio::test]
async fn header_access_failures_are_execution_errors() {
    let storage = HeaderStorage {
        files: [("repo/file".to_owned(), vec![])].into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let header_check = toml::from_str("[[rules]]\nglob = '**'\npattern = '^GOOD'").unwrap();
    let repositories = [RepositoryLocation {
        prefix: "repo",
        header_check: Some(&header_check),
    }];
    assert!(
        inspect(&storage, &repositories, Selection::default())
            .await
            .is_err()
    );
}

/// No object downloads or header diagnostics occur when content is disabled.
#[tokio::test]
async fn disabled_content_never_reads_objects() {
    let storage = HeaderStorage {
        files: [("repo/file".to_owned(), vec![])].into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let header_check = toml::from_str("[[rules]]\nglob = '**'\npattern = '^GOOD'").unwrap();
    let repositories = [RepositoryLocation {
        prefix: "repo",
        header_check: Some(&header_check),
    }];
    let checks = Selection::from_kinds(&[selection::Kind::Folders, selection::Kind::Age]);
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert!(!tree.failed(checks));
    assert_eq!(storage.reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(tree.header_diagnostics("s3://test", checks).is_empty());
}

/// Missing repositories are neutral when folder validation is not selected.
#[tokio::test]
async fn disabled_folders_do_not_fail_missing_repositories() {
    let storage = HeaderStorage {
        files: BTreeMap::new(),
        reads: 0.into(),
        fail_reads: false,
    };
    let repositories = [RepositoryLocation {
        prefix: "missing",
        header_check: None,
    }];
    let checks = Selection::from_kinds(&[selection::Kind::Age, selection::Kind::Content]);
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert!(!tree.failed(checks));
    assert!(tree.issues("s3://test", Utc::now(), 7, checks).is_empty());
    let rendered = tree.render("s3://test", Utc::now(), 7, true, checks);
    assert!(rendered.contains("missing  [not checked]"));
    assert!(!rendered.contains('\x1b'));
}

/// Disabled age checks never make an existing repository stale or yellow.
#[tokio::test]
async fn disabled_age_does_not_report_stale_or_unconfigured_entries() {
    let storage = HeaderStorage {
        files: [
            ("repo/file".to_owned(), b"GOOD".to_vec()),
            ("untracked/file".to_owned(), vec![]),
        ]
        .into(),
        reads: 0.into(),
        fail_reads: false,
    };
    let header_check = toml::from_str("[[rules]]\nglob = '**'\npattern = '^GOOD'").unwrap();
    let repositories = [RepositoryLocation {
        prefix: "repo",
        header_check: Some(&header_check),
    }];
    let now = Utc::now() + TimeDelta::days(8);
    let checks = Selection::from_kinds(&[selection::Kind::Content]);
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert!(!tree.failed(checks));
    assert!(tree.issues("s3://test", now, 7, checks).is_empty());
    let rendered = tree.render("s3://test", now, 7, true, checks);
    assert!(rendered.contains("\x1b[32mrepo"));
    assert!(!rendered.contains("\x1b[33m"));
    assert!(!rendered.contains("latest:"));
    assert!(!rendered.contains("untracked"));
}

/// Content without per-repository rules remains visibly unchecked, not green.
#[tokio::test]
async fn content_without_rules_is_neutral() {
    let storage = HeaderStorage {
        files: [("repo/file".to_owned(), vec![])].into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let repositories = [RepositoryLocation {
        prefix: "repo",
        header_check: None,
    }];
    let checks = Selection::from_kinds(&[selection::Kind::Content]);
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert!(!tree.failed(checks));
    assert!(
        !tree
            .render("s3://test", Utc::now(), 7, true, checks)
            .contains('\x1b')
    );
    assert_eq!(storage.reads.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// An entire-bucket repository renders as one line without exposing its contents.
#[tokio::test]
async fn bucket_root_repository_collapses_everything() {
    let storage = HeaderStorage {
        files: [("deep/file".to_owned(), vec![])].into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let repositories = [RepositoryLocation {
        prefix: "",
        header_check: None,
    }];
    let checks = Selection::default();
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert!(!tree.failed(checks));
    assert!(tree.issues("s3://test", Utc::now(), 7, checks).is_empty());
    assert_eq!(
        tree.render("s3://test", Utc::now(), 7, false, checks)
            .lines()
            .count(),
        1
    );
}

/// A single tree can be evaluated under different run-wide selections without mutation.
#[test]
fn tree_does_not_retain_check_selection() {
    let now = Utc::now();
    let mut tree = Folder::default();
    tree.at("parent/repo").repository = Some(Repository {
        exists: true,
        newest: Some(now - TimeDelta::days(10)),
        checked_files: 1,
        header_failures: 1,
        failure_samples: vec!["file: invalid header".to_owned()],
    });
    tree.at("parent/missing").repository = Some(Repository::default());
    tree.at("unconfigured");

    let folders = Selection::from_kinds(&[selection::Kind::Folders]);
    let age = Selection::from_kinds(&[selection::Kind::Age]);
    let content = Selection::from_kinds(&[selection::Kind::Content]);
    let repo = &tree.children["parent"].children["repo"];
    assert!(!repo.failed(folders));
    assert!(!repo.failed(age));
    assert!(repo.failed(content));
    assert!(tree.failed(folders));
    assert!(!tree.failed(age));

    let folder_issues = tree.issues("s3://test", now, 7, folders).join("\n");
    assert!(folder_issues.contains("[MISSING]"));
    assert!(folder_issues.contains("[UNCONFIGURED]"));
    assert!(!folder_issues.contains("[STALE]"));
    assert!(!folder_issues.contains("[HEADER CHECK FAILED]"));
    assert_eq!(tree.issues("s3://test", now, 7, age).len(), 1);
    assert!(tree.issues("s3://test", now, 7, age)[0].starts_with("[STALE]"));
    assert_eq!(tree.issues("s3://test", now, 7, content).len(), 1);
    assert!(tree.issues("s3://test", now, 7, content)[0].starts_with("[HEADER CHECK FAILED]"));
    assert!(tree.header_diagnostics("s3://test", folders).is_empty());
    assert_eq!(tree.header_diagnostics("s3://test", content).len(), 1);
    let rendered = tree.render("s3://test", now, 7, true, folders);
    assert!(rendered.contains("\x1b[32mrepo"));
    assert!(!rendered.contains("header failures"));
    assert!(!rendered.contains("latest:"));
}
