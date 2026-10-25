//! End-to-end provider selection and result isolation against local S3 servers.

use super::{EMAIL, run_args_with_env};
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Distinct credentials let request signatures prove which provider was used.
const CREDENTIALS: &[(&str, &str)] = &[
    ("TEST_MIRROR_ACCESS_KEY", "mirror-key"),
    ("TEST_MIRROR_SECRET_KEY", "mirror-secret"),
    ("TEST_MIRROR_SESSION_TOKEN", "mirror-token"),
];

/// One-response S3 server with bounded waits so routing failures cannot hang tests.
fn server(contents: &str) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let body =
        format!("<ListBucketResult><IsTruncated>false</IsTruncated>{contents}</ListBucketResult>");
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "provider was not contacted");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("{error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let count = socket.read(&mut buffer).unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
        }
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        String::from_utf8(request).unwrap()
    });
    (endpoint, handle)
}

/// Both providers share repository settings but use independent endpoints and credentials.
fn config(primary: &str, mirror: &str) -> String {
    format!(
        "[providers.primary]
type = 's3'
endpoint = '{primary}'
region = 'us-east-1'
access_key_id_env = 'AWS_ACCESS_KEY_ID'
secret_access_key_env = 'AWS_SECRET_ACCESS_KEY'
[providers.mirror]
type = 's3'
endpoint = '{mirror}'
region = 'mirror-region'
access_key_id_env = 'TEST_MIRROR_ACCESS_KEY'
secret_access_key_env = 'TEST_MIRROR_SECRET_KEY'
session_token_env = 'TEST_MIRROR_SESSION_TOKEN'
[[repositories]]
uri = 's3://test-bucket/repo'
{EMAIL}"
    )
}

/// Mirrored URIs must not merge healthy and missing findings in either command.
#[test]
fn all_providers_have_independent_results_and_credentials() {
    for args in [vec!["check", "--all"], vec!["report", "--dry-run"]] {
        let contents = format!(
            "<Contents><Key>repo/file</Key><LastModified>{}</LastModified><Size>0</Size></Contents>",
            chrono::Utc::now().to_rfc3339()
        );
        let (primary, primary_server) = server(&contents);
        let (mirror, mirror_server) = server("");
        let output = run_args_with_env(&config(&primary, &mirror), &args, CREDENTIALS);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        if args[0] == "check" {
            let (mirror, primary) = stdout.split_once("Provider: primary").unwrap();
            assert!(mirror.contains("Provider: mirror"));
            assert!(mirror.contains("[folders: not found]"));
            assert!(!primary.contains("not found"));
            assert!(primary.contains("[folders: found]"));
        } else {
            assert!(stdout.contains("[mirror] s3://test-bucket/repo: folders: not found"));
            assert!(!stdout.contains("[primary]"));
        }
        let primary_request = primary_server.join().unwrap();
        let mirror_request = mirror_server.join().unwrap();
        assert!(primary_request.contains("Credential=test/"));
        assert!(mirror_request.contains("Credential=mirror-key/"));
        assert!(mirror_request.contains("/mirror-region/s3/aws4_request"));
        assert!(
            mirror_request
                .to_ascii_lowercase()
                .contains("x-amz-security-token: mirror-token")
        );
    }
}

/// Selection works before or after the subcommand and ignores other providers' credentials.
#[test]
fn selection_skips_unselected_endpoints_and_credentials() {
    for (selected, args) in [
        ("primary", vec!["check", "--provider", "primary"]),
        (
            "primary",
            vec!["--provider", "primary", "report", "--dry-run"],
        ),
        ("mirror", vec!["check", "--provider", "mirror"]),
        (
            "mirror",
            vec!["report", "--dry-run", "--provider", "mirror"],
        ),
    ] {
        let (endpoint, handle) = server("");
        let (config, env) = if selected == "primary" {
            (
                config(&endpoint, "http://127.0.0.1:1"),
                vec![
                    ("TEST_MIRROR_ACCESS_KEY", ""),
                    ("TEST_MIRROR_SECRET_KEY", ""),
                ],
            )
        } else {
            let mut env = CREDENTIALS.to_vec();
            env.extend([("AWS_ACCESS_KEY_ID", ""), ("AWS_SECRET_ACCESS_KEY", "")]);
            (config("http://127.0.0.1:1", &endpoint), env)
        };
        let output = run_args_with_env(&config, &args, &env);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let other = if selected == "primary" {
            "mirror"
        } else {
            "primary"
        };
        assert!(stdout.contains(selected));
        assert!(!stdout.contains(other));
        handle.join().unwrap();
    }
}

/// Unknown names fail before credentials or storage are used, including for reports.
#[test]
fn unknown_provider_is_an_execution_error() {
    for command in ["check", "report"] {
        let output = run_args_with_env(
            &config("http://127.0.0.1:1", "http://127.0.0.1:1"),
            &[command, "--provider", "unknown"],
            &[],
        );
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("unknown provider: unknown")
        );
    }
}

/// A later provider's failure must suppress all earlier results and report previews.
#[test]
fn provider_failure_never_outputs_partial_results() {
    for args in [
        vec!["check"],
        vec!["report", "--dry-run", "--send-even-if-healthy"],
    ] {
        // Providers are visited in name order, so the mirror succeeds first.
        let (mirror, handle) = server("");
        let config = config("http://127.0.0.1:1", &mirror).replace(
            "access_key_id_env = 'AWS_ACCESS_KEY_ID'",
            "access_key_id_env = 'TEST_EMPTY_KEY'",
        );
        let mut env = CREDENTIALS.to_vec();
        env.push(("TEST_EMPTY_KEY", ""));
        let output = run_args_with_env(&config, &args, &env);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("cannot check provider primary")
        );
        handle.join().unwrap();
    }
}

/// Healthy confirmations identify every provider, even when their repository URIs match.
#[test]
fn healthy_report_identifies_both_providers() {
    let contents = format!(
        "<Contents><Key>repo/file</Key><LastModified>{}</LastModified><Size>0</Size></Contents>",
        chrono::Utc::now().to_rfc3339()
    );
    let (primary, primary_server) = server(&contents);
    let (mirror, mirror_server) = server(&contents);
    let output = run_args_with_env(
        &config(&primary, &mirror),
        &["report", "--dry-run", "--send-even-if-healthy"],
        CREDENTIALS,
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("All backup repositories are healthy."));
    for provider in ["primary", "mirror"] {
        assert!(stdout.contains(&format!(
            "[{provider}] s3://test-bucket/repo: folders: found"
        )));
    }
    primary_server.join().unwrap();
    mirror_server.join().unwrap();
}
