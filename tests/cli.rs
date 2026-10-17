use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Output},
};

fn run(config: &str) -> Output {
    run_args(config, &["view"])
}

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
        .env_remove("SMTP_USERNAME")
        .env_remove("SMTP_PASSWORD")
        .output()
        .unwrap()
}

fn view_with_listing(contents: &str) -> Output {
    with_listing(contents, &["view"], "")
}

fn with_listing(contents: &str, args: &[&str], extra_config: &str) -> Output {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let body =
        format!("<ListBucketResult><IsTruncated>false</IsTruncated>{contents}</ListBucketResult>");
    let server = std::thread::spawn(move || {
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
        write!(socket,
            "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        ).unwrap();
    });
    let output = run_args(
        &format!(
            "s3_endpoint = '{endpoint}'\ns3_region = 'us-east-1'\nrepositories = ['s3://test-bucket/repo']\n{extra_config}"
        ),
        args,
    );
    server.join().unwrap();
    output
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
    assert!(stdout.contains("[STALE] s3://test-bucket/repo"));
    assert!(stdout.contains("[UNCONFIGURED] s3://test-bucket/untracked"));
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
            .contains("[MISSING] s3://test-bucket/repo")
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

#[test]
fn report_requires_email_configuration() {
    let output = run_args(
        "s3_endpoint = 'https://example.com'\ns3_region = 'us-east-1'\nrepositories = ['s3://bucket/repo']",
        &["report", "--dry-run"],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("[email]")
    );
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
    let output = view_with_listing(
        "<Contents><Key>repo/deep/file</Key><LastModified>2000-01-01T00:00:00Z</LastModified></Contents><Contents><Key>untracked/file</Key><LastModified>2000-01-01T00:00:00Z</LastModified></Contents>",
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("repo  [latest:"));
    assert!(stdout.contains("untracked"));
    assert!(!stdout.contains("deep"));
    assert!(!stdout.contains('\x1b'));
}

#[test]
fn missing_repositories_exit_one() {
    let output = view_with_listing("");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("repo  [not found]")
    );
}

#[test]
fn overlapping_repositories_exit_two() {
    let output = run(
        "s3_endpoint = 'https://example.com'\ns3_region = 'us-east-1'\nrepositories = ['s3://bucket/repo', 's3://bucket/repo/child']",
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("overlapping repositories")
    );
    assert!(output.stdout.is_empty());
}
