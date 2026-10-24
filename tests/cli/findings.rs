//! Check and report share finding messages, including simultaneous failing checks.

use super::*;

/// Both commands expose identical age/content messages and the same failure exit code.
#[test]
fn check_and_report_use_the_same_findings() {
    let listing = b"<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>repo/deep/file</Key><LastModified>2000-01-01T00:00:00Z</LastModified></Contents></ListBucketResult>";
    let mut outputs = Vec::new();
    for args in [vec!["check"], vec!["report", "--dry-run"]] {
        let output = with_s3_responses(
            vec![(200, listing.to_vec()), (206, b"WRONG".to_vec())],
            &args,
            &format!("{CONTENT_RULES}\n{EMAIL}"),
        );
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        outputs.push(String::from_utf8(output.stdout).unwrap());
    }
    for message in [
        "age: latest is 2000-01-01T00:00:00",
        "content: 1 files do not match pattern",
    ] {
        assert!(outputs[0].contains(&format!("[{message}]")));
        assert!(
            outputs[1].lines().any(
                |line| line.starts_with(&format!("[primary] s3://test-bucket/repo: {message}"))
            )
        );
    }
    assert!(outputs[1].contains("2 problematic entries"));
    assert!(!outputs[1].contains(": folders: found"));
    assert!(outputs[1].contains("deep/file"));
}

/// Healthy reports format informational findings per line rather than building another tree.
#[test]
fn healthy_reports_list_findings_without_tree_decorations() {
    let contents = format!(
        "<Contents><Key>repo/file</Key><LastModified>{}</LastModified></Contents>",
        chrono::Utc::now().to_rfc3339()
    );
    let output = with_listing(
        &contents,
        &["report", "--dry-run", "--send-even-if-healthy"],
        EMAIL,
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout
            .lines()
            .any(|line| line == "[primary] s3://test-bucket/repo: folders: found")
    );
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("[primary] s3://test-bucket/repo: age: latest is "))
    );
    assert!(!stdout.contains("[folders:") && !stdout.contains("└──"));
}
