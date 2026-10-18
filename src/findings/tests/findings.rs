//! Shared evaluation is the only source of check messages and behavioral decisions.

use super::*;
use crate::findings::{Severity, selection::Kind};

/// Independent checks retain simultaneous findings, and rendering never evaluates them again.
#[test]
fn one_evaluation_drives_tree_report_and_exit_code() {
    let now = DateTime::parse_from_rfc3339("2026-10-18T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let mut tree = Folder::default();
    tree.at("repo").repository = Some(Repository {
        exists: true,
        newest: Some(now - TimeDelta::days(10)),
        checked_files: 3,
        content_failures: 2,
        failure_samples: vec!["bad.gz: content does not match pattern".to_owned()],
        silence_checks: vec![],
    });
    let results = super::super::findings(&tree, "s3://bucket", now, 7, Selection::default());
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].severity, Severity::Passed);
    assert_eq!(results[1].check, Kind::Age);
    assert_eq!(results[1].severity, Severity::Warning);
    assert_eq!(results[2].check, Kind::Content);
    assert!(results[2].fatal);
    let lines: Vec<_> = results
        .iter()
        .filter(|finding| finding.is_issue())
        .map(Finding::line)
        .collect();
    assert_eq!(
        lines,
        [
            "s3://bucket/repo: age: latest is 2026-10-08T00:00:00",
            "s3://bucket/repo: content: 2 files do not match pattern: bad.gz: content does not match pattern; additional failures omitted",
        ]
    );
    let before = crate::check::render(&tree, "s3://bucket", &results, true);
    for finding in &results {
        assert!(before.contains(&format!("[{}: {}]", finding.check.name(), finding.message)));
    }
    assert!(before.contains("\x1b[31mrepo"));
    assert!(!before.contains("\x1b[33mrepo"));
    // Changing measurements cannot change presentation without explicit re-evaluation.
    let repo = tree.at("repo").repository.as_mut().unwrap();
    repo.newest = Some(now);
    repo.content_failures = 0;
    assert_eq!(
        before,
        crate::check::render(&tree, "s3://bucket", &results, true)
    );
}

/// Red unconfigured folders trigger alerts but not failure exits; missing repositories do both.
#[test]
fn severity_and_exit_code_impact_are_independent() {
    let mut tree = Folder::default();
    tree.at("missing").repository = Some(Repository::default());
    tree.at("unknown/deep");
    let results = super::super::findings(&tree, "s3://bucket", Utc::now(), 7, Selection::default());
    let missing = results
        .iter()
        .find(|finding| finding.uri.ends_with("/missing"))
        .unwrap();
    assert!(missing.is_issue() && missing.fatal);
    for finding in results
        .iter()
        .filter(|finding| finding.uri.contains("/unknown"))
    {
        assert_eq!(finding.severity, Severity::Error);
        assert!(finding.is_issue());
        assert!(!finding.fatal);
    }
    let output = crate::check::render(&tree, "s3://bucket", &results, true);
    assert!(output.contains("\x1b[31;4mmissing"));
    assert!(output.contains("\x1b[31munknown"));
    assert!(!output.contains("deep"));
}
