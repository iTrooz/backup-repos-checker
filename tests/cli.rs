use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Output},
};

/// Check and report format the same independently evaluated findings.
#[path = "cli/findings.rs"]
mod findings;
/// Config-driven repository exclusions for both CLI commands.
#[path = "cli/silencing.rs"]
mod silencing;

fn run_args(config: &str, args: &[&str]) -> Output {
    let directory = if std::path::Path::new("/tmp/opencode").is_dir() {
        tempfile::tempdir_in("/tmp/opencode").unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    std::fs::write(directory.path().join("config.toml"), config).unwrap();
    Command::new(env!("CARGO_BIN_EXE_backup-repos-checker"))
        .args(args)
        .current_dir(directory.path())
        .env("AWS_ACCESS_KEY_ID", "test")
        .env("AWS_SECRET_ACCESS_KEY", "test")
        .env_remove("AWS_SESSION_TOKEN")
        .env("SMTP_USERNAME", "test")
        .env("SMTP_PASSWORD", "test")
        .output()
        .unwrap()
}

fn check_with_listing(contents: &str) -> Output {
    with_listing(contents, &["check"], "")
}

fn with_listing(contents: &str, args: &[&str], extra_config: &str) -> Output {
    let body =
        format!("<ListBucketResult><IsTruncated>false</IsTruncated>{contents}</ListBucketResult>");
    with_s3_responses(vec![(200, body.into_bytes())], args, extra_config)
}

fn with_s3_responses(responses: Vec<(u16, Vec<u8>)>, args: &[&str], extra_config: &str) -> Output {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for (index, (status, body)) in responses.into_iter().enumerate() {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let count = socket.read(&mut buffer).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
            }
            if index > 0 {
                assert!(
                    String::from_utf8_lossy(&request)
                        .to_ascii_lowercase()
                        .contains("range: bytes=0-63")
                );
            }
            write!(socket,
            "HTTP/1.1 {status} Response\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len(),
        ).unwrap();
            socket.write_all(&body).unwrap();
        }
    });
    let output = run_args(
        &format!(
            "s3_endpoint = '{endpoint}'\ns3_region = 'us-east-1'\n[[repositories]]\nuri = 's3://test-bucket/repo'\n{extra_config}"
        ),
        args,
    );
    server.join().unwrap();
    output
}

const CONTENT_RULES: &str = "[[repositories.content_check.rules]]\nglob = '**'\npattern = '^MAGIC'";

/// Run a recursive content check against a local S3 response sequence.
fn check_content(status: u16, bytes: &[u8], args: &[&str], extra: &str) -> Output {
    let listing = format!(
        "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>repo/deep/file</Key><LastModified>{}</LastModified></Contents></ListBucketResult>",
        chrono::Utc::now().to_rfc3339(),
    );
    with_s3_responses(
        vec![(200, listing.into_bytes()), (status, bytes.to_vec())],
        args,
        &format!("{CONTENT_RULES}\n{extra}"),
    )
}

/// Read failures are execution errors, while empty mismatching files are findings.
#[test]
fn content_access_errors_exit_two_but_empty_files_exit_one() {
    let output = check_content(
        403,
        b"<Error><Code>AccessDenied</Code></Error>",
        &["check"],
        "",
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    let output = check_content(
        416,
        b"<Error><Code>InvalidRange</Code></Error>",
        &["check"],
        "",
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
}

const EMAIL: &str = "[email]\nsmtp_host = '127.0.0.1'\nsmtp_port = 1\nfrom = 'backups@example.com'\nto = ['ops@example.com']";

#[test]
fn dry_run_previews_stale_and_unconfigured_entries_without_smtp() {
    let output = with_listing(
        "<Contents><Key>repo/deep/file</Key><LastModified>2000-01-01T00:00:00Z</LastModified></Contents><Contents><Key>untracked/file</Key><LastModified>2000-01-01T00:00:00Z</LastModified></Contents>",
        &["report", "--dry-run"],
        EMAIL,
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("To: ops@example.com"));
    assert!(stdout.contains("s3://test-bucket/repo: age: latest is 2000-01-01T00:00:00"));
    assert!(stdout.contains("s3://test-bucket/untracked: folders: unconfigured"));
    assert!(!stdout.contains("deep"));
    assert!(!stdout.contains('\x1b'));
}

#[test]
fn dry_run_reports_missing_repositories_and_exits_one() {
    let output = with_listing("", &["report", "--dry-run"], EMAIL);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("s3://test-bucket/repo: folders: not found")
    );
}

#[test]
fn healthy_report_skips_smtp() {
    let contents = format!(
        "<Contents><Key>repo/file</Key><LastModified>{}</LastModified></Contents>",
        chrono::Utc::now().to_rfc3339(),
    );
    let output = with_listing(&contents, &["report"], EMAIL);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("no email sent")
    );
}

/// The new flag preserves alerts for missing, stale, or unconfigured entries.
#[test]
fn healthy_flag_does_not_hide_problems() {
    for (contents, code, issue) in [
        ("", 1, ": folders: not found"),
        (
            "<Contents><Key>repo/file</Key><LastModified>2000-01-01T00:00:00Z</LastModified></Contents>",
            0,
            ": age: latest is ",
        ),
        (
            "<Contents><Key>repo/file</Key><LastModified>2100-01-01T00:00:00Z</LastModified></Contents><Contents><Key>unknown/file</Key><LastModified>2100-01-01T00:00:00Z</LastModified></Contents>",
            0,
            ": folders: unconfigured",
        ),
    ] {
        let output = with_listing(
            contents,
            &["report", "--send-even-if-healthy", "--dry-run"],
            EMAIL,
        );
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("Subject: Backup repository alerts"));
        assert!(stdout.contains(issue));
        assert!(!stdout.contains("All backup repositories are healthy."));
    }
}

/// Non-preview healthy reports attempt SMTP delivery and propagate delivery errors.
#[test]
fn healthy_report_attempts_smtp_delivery() {
    let contents = format!(
        "<Contents><Key>repo/file</Key><LastModified>{}</LastModified></Contents>",
        chrono::Utc::now().to_rfc3339(),
    );
    let output = with_listing(&contents, &["report", "--send-even-if-healthy"], EMAIL);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("cannot send report email")
    );
}

/// A failed inventory must never produce a healthy confirmation.
#[test]
fn healthy_flag_does_not_report_after_scan_errors() {
    let output = with_s3_responses(
        vec![(
            200,
            b"<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>".to_vec(),
        )],
        &["report", "--send-even-if-healthy", "--dry-run"],
        EMAIL,
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
}

#[test]
fn report_uses_nested_age_threshold() {
    let contents = format!(
        "<Contents><Key>repo/file</Key><LastModified>{}</LastModified></Contents>",
        (chrono::Utc::now() - chrono::TimeDelta::days(8)).to_rfc3339(),
    );
    let output = with_listing(
        &contents,
        &["report", "--dry-run"],
        &format!("[checks.age]\nmax_age_days = 14\n{EMAIL}"),
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("no email sent")
    );
    let output = with_listing(
        &contents,
        &["report", "--dry-run"],
        &format!("[checks.age]\nmax_age_days = 3\n{EMAIL}"),
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("s3://test-bucket/repo: age: latest is "));
    assert!(stdout.contains("Freshness threshold: 3 days"));
}

#[test]
fn smtp_without_required_starttls_fails_without_sending() {
    use std::io::{BufRead, BufReader};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        socket.write_all(b"220 localhost ESMTP\r\n").unwrap();
        let mut command = String::new();
        BufReader::new(socket.try_clone().unwrap())
            .read_line(&mut command)
            .unwrap();
        assert!(command.starts_with("EHLO "), "{command}");
        // No STARTTLS capability: the client must refuse to transmit the email.
        socket.write_all(b"250 localhost\r\n").unwrap();
    });
    let email = EMAIL.replace("smtp_port = 1", &format!("smtp_port = {port}"));
    let output = with_listing("", &["report"], &email);
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("cannot send report email")
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn stale_and_unconfigured_folders_exit_successfully() {
    let output = check_with_listing(
        "<Contents><Key>repo/deep/file</Key><LastModified>2000-01-01T00:00:00Z</LastModified></Contents><Contents><Key>untracked/file</Key><LastModified>2000-01-01T00:00:00Z</LastModified></Contents>",
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("repo  [age: latest is "));
    assert!(stdout.contains("untracked"));
    assert!(!stdout.contains("deep"));
    assert!(!stdout.contains('\x1b'));
}

#[test]
fn missing_repositories_exit_one() {
    let output = check_with_listing("");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("repo  [folders: not found]")
    );
}

/// Selected content checks still download and validate file prefixes.
#[test]
fn content_selection_preserves_validation_failures() {
    for checks in [
        "content",
        "folders,content",
        "folders,age,content",
        "content,content",
    ] {
        let output = check_content(206, b"WRONG", &["check", "--all", "--checks", checks], "");
        assert_eq!(output.status.code(), Some(1), "{checks}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("[content: 1 files do not match pattern]"));
        assert_eq!(stdout.contains("[age:"), checks.contains("age"));
    }
}

/// Only selecting folder validation makes a missing repository exit nonzero.
#[test]
fn missing_repository_exit_depends_on_folder_selection() {
    for (checks, expected) in [
        ("age", 0),
        ("content", 0),
        ("age,content", 0),
        ("folders", 1),
        ("folders,age", 1),
    ] {
        let output = with_listing("", &["check", "--checks", checks], "");
        assert_eq!(output.status.code(), Some(expected), "{checks}: {output:?}");
    }
}

/// Reject unknown and empty names before reading configuration or contacting S3.
#[test]
fn invalid_check_names_are_usage_errors() {
    for checks in ["", "unknown", "folders,", "age,unknown"] {
        let output = run_args("", &["check", "--checks", checks]);
        assert_eq!(output.status.code(), Some(2), "{checks}: {output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains("invalid value") || stderr.contains("a value is required"),
            "{checks:?}: {stderr}"
        );
        assert!(!stderr.contains("config.toml"));
    }
    let output = run_args("", &["check", "--checks"]);
    assert_eq!(output.status.code(), Some(2));
}
