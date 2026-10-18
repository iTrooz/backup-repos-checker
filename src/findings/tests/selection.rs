//! Independent check selection and re-evaluation of stored findings.

use super::*;
use crate::findings::selection::Kind;

/// No downloads or content diagnostics occur when content is disabled.
#[tokio::test]
async fn disabled_content_never_reads_objects() {
    let storage = TestStorage {
        files: [("repo/file".to_owned(), vec![])].into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let content_check = toml::from_str("[[rules]]\nglob = '**'\npattern = '^GOOD'").unwrap();
    let repositories = [RepositoryLocation {
        prefix: "repo",
        content_check: Some(&content_check),
        silence_checks: &[],
    }];
    let checks = Selection::from_kinds(&[Kind::Folders, Kind::Age]);
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert!(!tree.failed(checks));
    assert_eq!(storage.reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(tree.content_diagnostics("s3://test", checks).is_empty());
}

/// Missing repositories are neutral when folder validation is not selected.
#[tokio::test]
async fn disabled_folders_do_not_fail_missing_repositories() {
    let storage = TestStorage {
        files: BTreeMap::new(),
        reads: 0.into(),
        fail_reads: false,
    };
    let repositories = [RepositoryLocation {
        prefix: "missing",
        content_check: None,
        silence_checks: &[],
    }];
    let checks = Selection::from_kinds(&[Kind::Age, Kind::Content]);
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert!(!tree.failed(checks));
    assert!(tree.issues("s3://test", Utc::now(), 7, checks).is_empty());
    let rendered = tree.render("s3://test", Utc::now(), 7, true, checks);
    assert!(rendered.contains("missing (0 files)\n"));
    assert!(!rendered.contains("[folders:"));
    assert!(!rendered.contains('\x1b'));
}

/// Disabled age checks never make an existing repository stale or yellow.
#[tokio::test]
async fn disabled_age_does_not_report_stale_or_unconfigured_entries() {
    let storage = TestStorage {
        files: [
            ("repo/file".to_owned(), b"GOOD".to_vec()),
            ("untracked/file".to_owned(), vec![]),
        ]
        .into(),
        reads: 0.into(),
        fail_reads: false,
    };
    let content_check = toml::from_str("[[rules]]\nglob = '**'\npattern = '^GOOD'").unwrap();
    let repositories = [RepositoryLocation {
        prefix: "repo",
        content_check: Some(&content_check),
        silence_checks: &[],
    }];
    let now = Utc::now() + TimeDelta::days(8);
    let checks = Selection::from_kinds(&[Kind::Content]);
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert!(!tree.failed(checks));
    assert!(tree.issues("s3://test", now, 7, checks).is_empty());
    let rendered = tree.render("s3://test", now, 7, true, checks);
    assert!(rendered.contains("\x1b[32mrepo"));
    for omitted in ["\x1b[33m", "[age:", "[folders:", "untracked"] {
        assert!(!rendered.contains(omitted));
    }
}

/// Content without repository rules is unchecked, not falsely marked green.
#[tokio::test]
async fn content_without_rules_is_neutral() {
    let storage = TestStorage {
        files: [("repo/file".to_owned(), vec![])].into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let repositories = [RepositoryLocation {
        prefix: "repo",
        content_check: None,
        silence_checks: &[],
    }];
    let checks = Selection::from_kinds(&[Kind::Content]);
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert!(!tree.failed(checks));
    assert!(
        !tree
            .render("s3://test", Utc::now(), 7, true, checks)
            .contains('\x1b')
    );
    assert_eq!(storage.reads.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// One result tree can be evaluated with different run-wide selections without mutation.
#[test]
fn tree_does_not_retain_check_selection() {
    let now = Utc::now();
    let mut tree = Folder::default();
    tree.at("parent/repo").repository = Some(Repository {
        exists: true,
        newest: Some(now - TimeDelta::days(10)),
        checked_files: 1,
        content_failures: 1,
        failure_samples: vec!["file: invalid content".to_owned()],
        silence_checks: vec![],
    });
    tree.at("parent/missing").repository = Some(Repository::default());
    tree.at("unconfigured");
    let folders = Selection::from_kinds(&[Kind::Folders]);
    let age = Selection::from_kinds(&[Kind::Age]);
    let content = Selection::from_kinds(&[Kind::Content]);
    let repo = &tree.children["parent"].children["repo"];
    assert!(!repo.failed(folders));
    assert!(!repo.failed(age));
    assert!(repo.failed(content));
    assert!(tree.failed(folders));
    assert!(!tree.failed(age));
    let folder_issues = tree.issues("s3://test", now, 7, folders).join("\n");
    assert!(folder_issues.contains(": folders: not found"));
    assert!(folder_issues.contains(": folders: unconfigured"));
    assert!(!folder_issues.contains(": age:"));
    assert!(!folder_issues.contains(": content:"));
    assert_eq!(tree.issues("s3://test", now, 7, age).len(), 1);
    assert!(tree.issues("s3://test", now, 7, age)[0].starts_with("s3://test/parent/repo: age:"));
    assert_eq!(tree.issues("s3://test", now, 7, content).len(), 1);
    assert!(
        tree.issues("s3://test", now, 7, content)[0].starts_with("s3://test/parent/repo: content:")
    );
    assert!(tree.content_diagnostics("s3://test", folders).is_empty());
    assert_eq!(tree.content_diagnostics("s3://test", content).len(), 1);
    let rendered = tree.render("s3://test", now, 7, true, folders);
    assert!(rendered.contains("\x1b[32mrepo"));
    assert!(!rendered.contains("[content:"));
    assert!(!rendered.contains("[age:"));
}
