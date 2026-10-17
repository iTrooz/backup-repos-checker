# backup-repos-checker

Checks recent object activity in explicitly configured S3 backup repositories.
This does **not** verify backup integrity or successful backup completion.

## Usage

Copy `config.example.toml` to `config.toml` in the working directory and edit it.
Set `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` (and, for temporary
credentials, `AWS_SESSION_TOKEN`). Credentials are read only from environment
variables, not AWS profiles or instance metadata.

```sh
cargo run --release -- view
```

The custom endpoint uses path-style S3 requests. HTTP endpoints are supported for
local services; prefer HTTPS for remote endpoints. The account needs bucket-list
permission for every configured bucket; object downloads are not required.
Every configured bucket is listed in full, with automatic pagination.

Repository locations use `s3://bucket/folder` (literal S3 keys, not URL-encoded
paths); `s3://bucket` configures an entire bucket. Duplicate or overlapping
repositories in a bucket are rejected. A trailing slash is optional.
`max_age_days` defaults to 7 and must be positive.

`view` displays a folder tree with collapsed repository entries:

- Green: a file anywhere in the repository was modified within the threshold.
- Yellow: no recent files, or only directory markers exist.
- Red: unconfigured folders.
- Red and underlined: configured repositories that do not exist.
- Default color: ancestor folders containing configured repositories.

Timestamps are UTC. Modification exactly at the threshold counts as recent.
Directory markers establish existence but do not count as file activity.
A bare object named `repo` does not establish a repository at `repo/`.
Object files outside repositories are not displayed; their folders are.
Colors are disabled for non-terminal output or when `NO_COLOR` is set.

## Email reports

Add the `[email]` section from `config.example.toml`, then run:

```sh
cargo run --release -- report --dry-run
cargo run --release -- report
```

`report` sends one plain-text email listing all red and yellow entries across
all configured buckets, using full S3 paths and explicit status labels. Fresh
repositories and grouping folders are omitted, and repository contents remain
hidden. Each unconfigured folder is listed, including nested folders, just like
the red entries in `view`. No email is sent when there are no issues. No report
is sent if any bucket listing fails.

`--dry-run` is a flag on `report`: it still queries S3, but prints the email
headers and body without connecting to SMTP or requiring SMTP credentials.
An `[email]` section is required for previews too; `view` does not require one.

Set both `SMTP_USERNAME` and `SMTP_PASSWORD` for SMTP authentication, or leave
both unset for an unauthenticated relay. Credentials never appear in previews.
SMTP always uses verified TLS: `encryption = "starttls"` requires STARTTLS
(default port 587), while `encryption = "tls"` uses implicit TLS (default port
465). `smtp_port` can override these defaults. `to` accepts multiple recipients;
`subject` defaults to `Backup repository alerts`.

Exit codes for both commands (including dry runs): **0** for a completed check with no missing repositories (even when
stale or unconfigured folders exist), **1** for missing repositories, **2** for
configuration, credentials, network, email delivery, or other execution errors.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

`src/storage.rs` defines the recursive storage interface; additional backends can
implement it without changing freshness checks or rendering in `src/view.rs`.
