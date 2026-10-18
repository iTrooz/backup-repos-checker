//! Recursive content validation, failure counts, and storage error handling.

use super::*;

/// Match recursively, skip unmatched files and markers, and reject invalid matching files.
#[tokio::test]
async fn content_checks_are_recursive_and_skip_unmatched_files() {
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
        silence_checks: &[],
    }];

    let tree = inspect(&storage, &repositories, Selection::default())
        .await
        .unwrap();

    assert!(tree.failed(Selection::default()));
    assert_eq!(storage.reads.load(std::sync::atomic::Ordering::SeqCst), 3);

    let rendered = tree.render("s3://test", Utc::now(), 7, true, Selection::default());
    assert!(rendered.contains("\x1b[31mrepo"));
    assert!(rendered.contains("[content: 2 files do not match pattern]"));
    assert!(!rendered.contains("bad.gz"));

    let issues = tree
        .issues("s3://test", Utc::now(), 7, Selection::default())
        .join("\n");
    assert!(issues.contains("s3://test/repo: content: 2 files do not match pattern"));
    assert!(issues.contains("deep/bad.gz"));
    assert!(!issues.contains("unmatched.txt"));
    assert!(!issues.contains("root.gz"));
    assert!(!issues.contains(": age:"));
}

/// Unmatched files succeed even when downloading them would fail.
#[tokio::test]
async fn unmatched_files_are_accepted_without_downloads() {
    let storage = TestStorage {
        files: [
            ("repo/.resource/blob".to_owned(), vec![]),
            ("repo/info.json".to_owned(), vec![]),
        ]
        .into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let content_check = toml::from_str("[[rules]]\nglob = '**/*.md'\npattern = '^id: '").unwrap();
    let repositories = [RepositoryLocation {
        prefix: "repo",
        content_check: Some(&content_check),
        silence_checks: &[],
    }];
    let checks = Selection::default();

    let tree = inspect(&storage, &repositories, checks).await.unwrap();

    assert!(!tree.failed(checks));
    assert!(tree.content_diagnostics("s3://test", checks).is_empty());
    assert_eq!(storage.reads.load(std::sync::atomic::Ordering::SeqCst), 0);
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
        silence_checks: &[],
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
