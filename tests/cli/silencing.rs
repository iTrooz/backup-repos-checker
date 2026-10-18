//! Exercise repository silencing through configuration and real CLI execution.

use super::*;

/// Explicit CLI selection cannot re-enable content checks silenced in configuration.
#[test]
fn silenced_content_does_not_download_files() {
    let contents = format!(
        "<Contents><Key>repo/file</Key><LastModified>{}</LastModified></Contents>",
        chrono::Utc::now().to_rfc3339()
    );
    let output = with_listing(
        &contents,
        &["check", "--checks", "content"],
        &format!("silence_checks = ['content']\n{CONTENT_RULES}"),
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("repo (1 files)"));
    assert!(!stdout.contains("[content:"));
    assert!(output.stderr.is_empty());
}

/// Reports respect repository exclusions, including when deciding whether healthy mail is due.
#[test]
fn silenced_age_is_not_an_alert() {
    let contents = "<Contents><Key>repo/file</Key><LastModified>2000-01-01T00:00:00Z</LastModified></Contents>";
    let config = format!("silence_checks = ['age']\n{EMAIL}");
    let output = with_listing(contents, &["report", "--dry-run"], &config);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("no email sent")
    );
    let output = with_listing(
        contents,
        &["report", "--dry-run", "--send-even-if-healthy"],
        &config,
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("All backup repositories are healthy."));
    assert!(!stdout.contains("[age:") && !stdout.contains("[STALE]"));
}

/// Silencing folder validation makes a missing repository neutral and successful.
#[test]
fn silenced_missing_repository_does_not_fail() {
    let output = with_listing("", &["check"], "silence_checks = ['folders']");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("repo (0 files)"));
    assert!(!stdout.contains("not found"));
}

/// Configuration mistakes fail rather than silently leaving a check enabled.
#[test]
fn unknown_silenced_check_is_a_configuration_error() {
    let output = run_args(
        "s3_endpoint = 'https://example.com'\ns3_region = 'test'\n[[repositories]]\nuri = 's3://test/repo'\nsilence_checks = ['typo']",
        &["check"],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("unknown variant")
    );
}
