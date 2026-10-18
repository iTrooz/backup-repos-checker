//! Repository-local exclusions must apply independently of the run-wide selection.

use super::*;
use crate::check::selection::Kind;

/// Skip work and presentation only for the silenced repository; preserve counts and siblings.
#[tokio::test]
async fn silencing_is_local_and_skips_execution() {
    let storage = TestStorage {
        files: [
            ("group/silent/deep/file".to_owned(), vec![]),
            ("group/active/file".to_owned(), vec![]),
            ("unknown/file".to_owned(), vec![]),
        ]
        .into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let content_check = toml::from_str("[[rules]]\nglob = '**'\npattern = '^GOOD'").unwrap();
    let repositories = [
        RepositoryLocation {
            prefix: "group/silent",
            content_check: Some(&content_check),
            silence_checks: &[Kind::Folders, Kind::Age, Kind::Content],
        },
        RepositoryLocation {
            prefix: "group/active",
            content_check: None,
            silence_checks: &[],
        },
        RepositoryLocation {
            prefix: "group/missing",
            content_check: None,
            silence_checks: &[Kind::Folders],
        },
    ];
    let checks = Selection::default();
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    let repo = tree.children["group"].children["silent"]
        .repository
        .as_ref()
        .unwrap();
    assert!(repo.exists);
    assert!(repo.newest.is_none());
    assert_eq!(repo.checked_files, 0);
    assert_eq!(storage.reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!tree.failed(checks));
    let now = Utc::now() + TimeDelta::days(8);
    let output = tree.render("s3://test", now, 7, true, checks);
    assert!(output.contains("── silent (1 files)\n"));
    assert!(output.contains("── missing (0 files)\n"));
    assert!(output.contains("\x1b[33mactive"));
    let issues = tree.issues("s3://test", now, 7, checks).join("\n");
    assert!(issues.contains("[STALE] s3://test/group/active"));
    assert!(issues.contains("[UNCONFIGURED] s3://test/unknown"));
    assert!(!issues.contains("silent"));
    assert!(!issues.contains("missing"));
    assert!(tree.content_diagnostics("s3://test", checks).is_empty());
}

/// Silenced checks cannot reappear when evaluating previously collected findings.
#[test]
fn silencing_masks_stored_findings_in_every_output() {
    let now = Utc::now();
    let mut tree = Folder {
        repository: Some(Repository {
            exists: true,
            newest: Some(now - TimeDelta::days(10)),
            checked_files: 1,
            content_failures: 1,
            failure_samples: vec!["bad file".to_owned()],
            silence_checks: vec![Kind::Age],
        }),
        ..Folder::default()
    };
    let checks = Selection::default();
    let issues = tree.issues("s3://test", now, 7, checks);
    assert_eq!(issues.len(), 1);
    assert!(issues[0].contains("[CONTENT CHECK FAILED]"));
    tree.repository
        .as_mut()
        .unwrap()
        .silence_checks
        .push(Kind::Content);
    assert!(!tree.failed(checks));
    assert!(tree.issues("s3://test", now, 7, checks).is_empty());
    assert!(tree.content_diagnostics("s3://test", checks).is_empty());
    let output = tree.render("s3://test", now, 7, true, checks);
    assert!(!output.contains("[age:") && !output.contains("[content:"));
    assert!(output.contains("\x1b[32ms3://test"));
    assert!(
        tree.issues("s3://test", now, 7, Selection::from_kinds(&[Kind::Content]))
            .is_empty()
    );
}
