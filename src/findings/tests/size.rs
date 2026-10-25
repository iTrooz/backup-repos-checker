//! Repository byte totals, optional per-repository limits, and size display.

use super::*;
use crate::findings::selection::Kind;

/// Sizes exclude directory markers, apply only to the configured repository, and stay visible.
#[tokio::test]
async fn repository_size_is_measured_limited_and_always_displayed() {
    let storage = TestStorage {
        files: [
            ("repo/a".to_owned(), vec![0; 1536]),
            ("repo/deep/b".to_owned(), vec![0; 512]),
            ("repo/marker/".to_owned(), vec![0; 10]),
            ("other/file".to_owned(), vec![0; 4096]),
        ]
        .into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let checks = Selection::from_kinds(&[Kind::Folders, Kind::Size]);

    let repositories = [RepositoryLocation {
        prefix: "repo",
        content_check: None,
        max_size: Some(1536),
        silence_checks: &[],
    }];
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert_eq!(
        tree.children["repo"].repository.as_ref().unwrap().size,
        2048
    );
    assert!(tree.failed(checks));

    let issues = tree.issues("s3://test", Utc::now(), 7, checks).join("\n");
    assert!(
        issues.contains("s3://test/repo: size: 2 KiB exceeds limit 1.5 KiB"),
        "{issues}"
    );

    // Display mirrors file counts: repositories, unconfigured folders, and their measured bytes.
    let output = tree.render("s3://test", Utc::now(), 7, false, checks);
    assert!(
        output.contains(
            "repo  [folders: found]  [size: 2 KiB exceeds limit 1.5 KiB] (2 files, 2 KiB)"
        ),
        "{output}"
    );
    assert!(
        output.contains("other  [folders: unconfigured] (1 files, 4 KiB)"),
        "{output}"
    );

    // Silencing the finding or omitting the check never hides the measured size.
    let repositories = [RepositoryLocation {
        prefix: "repo",
        content_check: None,
        max_size: Some(1536),
        silence_checks: &[Kind::Size],
    }];
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert!(!tree.failed(checks));
    assert!(
        !tree
            .issues("s3://test", Utc::now(), 7, checks)
            .join("\n")
            .contains(": size:")
    );

    let output = tree.render(
        "s3://test",
        Utc::now(),
        7,
        false,
        Selection::from_kinds(&[Kind::Folders]),
    );
    assert!(!output.contains("[size:"), "{output}");
    assert!(output.contains("(2 files, 2 KiB)"), "{output}");
}

/// Limits pass at equality and produce no finding when the repository has no configured limit.
#[tokio::test]
async fn size_limit_passes_at_the_boundary_and_is_absent_without_configuration() {
    let storage = TestStorage {
        files: [("repo/file".to_owned(), vec![0; 1024])].into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let checks = Selection::default();

    let repositories = [RepositoryLocation {
        prefix: "repo",
        content_check: None,
        max_size: Some(1024),
        silence_checks: &[],
    }];
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert!(tree.issues("s3://test", Utc::now(), 7, checks).is_empty());
    assert!(
        tree.render("s3://test", Utc::now(), 7, false, checks)
            .contains("[size: 1 KiB (limit 1 KiB)]")
    );

    let repositories = [RepositoryLocation {
        prefix: "repo",
        content_check: None,
        max_size: None,
        silence_checks: &[],
    }];
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    let output = tree.render("s3://test", Utc::now(), 7, false, checks);
    assert!(!output.contains("[size:"), "{output}");
    assert!(output.contains("(1 files, 1 KiB)"), "{output}");
}
