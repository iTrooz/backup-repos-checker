//! Check filters only bracketed presentation, never scanning or failure detection.

use super::*;

/// Healthy findings are hidden by default; both flag spellings restore all details.
#[test]
fn informational_brackets_require_all_flag() {
    for args in [vec!["check"], vec!["check", "--all"], vec!["check", "-A"]] {
        let output = check_content(206, b"MAGIC123", &args, "");
        let all = args.len() > 1;

        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stderr.is_empty());

        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("repo"));
        assert!(stdout.contains("(1 files)"));
        for detail in [
            "[folders: found]",
            "[age: latest is ",
            "[content: 0 files do not match pattern]",
        ] {
            assert_eq!(stdout.contains(detail), all, "{stdout}");
        }
    }
}

/// Failed checks stay visible and fatal even when passing check details are hidden.
#[test]
fn default_output_keeps_issues_and_failure_diagnostics() {
    let output = check_content(206, b"WRONG", &["check"], "");
    assert_eq!(output.status.code(), Some(1), "{output:?}");

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("[content: 1 files do not match pattern]"));
    assert!(!stdout.contains("[age:") && !stdout.contains("[folders:"));
    assert!(stdout.contains("(1 files)"));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("deep/file")
    );
}
