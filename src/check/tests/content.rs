//! Recursive content validation, failure counts, and storage error handling.

use super::*;

/// Match recursively, ignore markers, and fail unmatched or invalid files.
#[tokio::test]
async fn content_checks_are_recursive_and_fail_closed() {
    let storage = TestStorage {
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
    let content_check =
        toml::from_str("[[rules]]\nglob = '**/*.gz'\npattern = '^\\x1f\\x8b\\x08'").unwrap();
    let repositories = [RepositoryLocation {
        prefix: "repo",
        content_check: Some(&content_check),
    }];
    let tree = inspect(&storage, &repositories, Selection::default())
        .await
        .unwrap();
    assert!(tree.failed(Selection::default()));
    assert_eq!(storage.reads.load(std::sync::atomic::Ordering::SeqCst), 3);
    let rendered = tree.render("s3://test", Utc::now(), 7, true, Selection::default());
    assert!(rendered.contains("\x1b[31mrepo"));
    assert!(rendered.contains("[content failures: 3/4]"));
    assert!(!rendered.contains("bad.gz"));
    let issues = tree
        .issues("s3://test", Utc::now(), 7, Selection::default())
        .join("\n");
    assert!(issues.contains("[CONTENT CHECK FAILED] s3://test/repo"));
    assert!(issues.contains("deep/bad.gz"));
    assert!(issues.contains("no matching content rule"));
    assert!(!issues.contains("root.gz"));
    assert!(!issues.contains("[STALE]"));
}

/// Continue checking all files while capping diagnostic samples.
#[tokio::test]
async fn checks_every_file_but_bounds_failure_details() {
    let storage = TestStorage {
        files: (0..27)
            .map(|i| (format!("deep/file{i}"), b"bad".to_vec()))
            .collect(),
        reads: 0.into(),
        fail_reads: false,
    };
    let content_check = toml::from_str("[[rules]]\nglob = '**'\npattern = '^GOOD'").unwrap();
    let repositories = [RepositoryLocation {
        prefix: "",
        content_check: Some(&content_check),
    }];
    let tree = inspect(&storage, &repositories, Selection::default())
        .await
        .unwrap();
    assert_eq!(storage.reads.load(std::sync::atomic::Ordering::SeqCst), 27);
    let repo = tree.repository.as_ref().unwrap();
    assert_eq!(repo.content_failures, 27);
    assert_eq!(repo.failure_samples.len(), MAX_FAILURE_SAMPLES);
    assert!(
        tree.issues("s3://test", Utc::now(), 7, Selection::default())[0]
            .contains("additional failures omitted")
    );
}

/// Distinguish failed downloads from files that fail content validation.
#[tokio::test]
async fn content_access_failures_are_execution_errors() {
    let storage = TestStorage {
        files: [("repo/file".to_owned(), vec![])].into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let content_check = toml::from_str("[[rules]]\nglob = '**'\npattern = '^GOOD'").unwrap();
    let repositories = [RepositoryLocation {
        prefix: "repo",
        content_check: Some(&content_check),
    }];
    assert!(
        inspect(&storage, &repositories, Selection::default())
            .await
            .is_err()
    );
}
