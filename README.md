# backup-repos-checker

Checks recent object activity and optionally file headers in explicitly configured
S3 backup repositories. This does **not** verify full backup integrity or successful
backup completion.

## Usage

Copy `config.example.toml` to `config.toml` in the working directory and edit it.
Set `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` (and, for temporary
credentials, `AWS_SESSION_TOKEN`). Credentials are read only from environment
variables, not AWS profiles or instance metadata.

```sh
cargo run --release -- check
```

The custom endpoint uses path-style S3 requests. HTTP endpoints are supported for
local services; prefer HTTPS for remote endpoints. The account needs bucket-list
permission for every configured bucket. Opt-in header checks additionally require
object-read permission; freshness-only checks do not download objects.
Every configured bucket is listed in full, with automatic pagination.

Repository locations use `s3://bucket/folder` (literal S3 keys, not URL-encoded
paths); `s3://bucket` configures an entire bucket. Duplicate or overlapping
repositories in a bucket are rejected. A trailing slash is optional.
Configure the freshness threshold separately:

```toml
[checks.age]
max_age_days = 7
```

`checks.age.max_age_days` defaults to 7 and must be positive. The section may
be omitted. Move any existing top-level `max_age_days` into `[checks.age]`.

`check` displays a folder tree with collapsed repository entries:

- Green: a file anywhere in the repository was modified within the threshold.
- Yellow: no recent files, or only directory markers exist.
- Red: unconfigured folders or repositories with failed header validation.
- Red and underlined: configured repositories that do not exist.
- Default color: ancestor folders containing configured repositories.

Timestamps are UTC. Modification exactly at the threshold counts as recent.
Directory markers establish existence but do not count as file activity.
A bare object named `repo` does not establish a repository at `repo/`.
Object files outside repositories are not displayed; their folders are.
Colors are disabled for non-terminal output or when `NO_COLOR` is set.

## Optional file-header checks

Repositories use tables rather than a list of strings. To migrate an old config,
replace each repository string with a `[[repositories]]` table and `uri` field.
The `view` subcommand has been renamed to `check` (no compatibility alias).

```toml
[[repositories]]
uri = "s3://bucket/backups/server-a"
# Freshness only: omit header_check entirely.

[[repositories]]
uri = "s3://bucket/archive"

[[repositories.header_check.rules]]
glob = "**/*.gz"
pattern = '^\x1f\x8b\x08'

[[repositories.header_check.rules]]
glob = "**/*.custom"
pattern = '^BACKUP v[12]\x00'
```

Header checks apply in both `check` and `report`, including `report --dry-run`.
Every file in an opted-in repository is checked recursively, including old files.
Directory markers are ignored. Rule globs match paths relative to the repository;
`**/*.gz` includes root-level and nested files, and `**` matches every file.
The first matching rule wins. A file without a matching rule fails validation.
Rules and patterns are validated before any S3 requests.

Patterns are Rust byte regular expressions, not Unicode expressions. TOML single
quotes preserve backslashes. Use `^` to anchor at the start, `\xNN` for an exact
byte, `(A|B)` for alternatives, and `.` for any byte (including newlines). For
example, `^(MAGIC_A|MAGIC_B).{4}\x00` matches either signature followed by four
arbitrary bytes and a zero byte. Patterns examine **only the first 64 bytes**;
the limit is hardcoded as `HEADER_BYTES` in `src/header.rs`, not configurable.
`$` refers to the end of this sampled prefix, not necessarily the entire file.
Short and empty files are matched against the bytes actually present.

Matching files are read with S3 range requests, with at most eight checks in
flight. Expect approximately one additional GET per matched file on each run.
Header failures turn repositories red, even if fresh, and exit with code 1.
The tree keeps repository contents collapsed and displays failure counts;
stderr diagnostics and emails include at most ten offending paths per repository.
The scan still checks every file after finding invalid headers. Read/access errors
abort the run with code 2 instead of being reported as corrupt files.

These are signature checks, not complete format or integrity checks. Encrypted
backup objects may have random-looking headers with no meaningful fixed pattern.

## Email reports

Add the `[email]` section from `config.example.toml`, then run:

```sh
cargo run --release -- report --dry-run
cargo run --release -- report
```

`report` sends one plain-text email listing all red and yellow entries across
all configured buckets, using full S3 paths and explicit status labels. Fresh
repositories with valid headers and grouping folders are omitted. Header failures
include bounded file-path diagnostics. Each unconfigured folder is listed,
including nested folders, just like the red entries in `check`. No email is sent
when there are no issues. No report is sent if any bucket listing or header read fails.

`--dry-run` is a flag on `report`: it still queries S3, but prints the email
headers and body without connecting to SMTP or requiring SMTP credentials.
An `[email]` section is required for previews too; `check` does not require one.

Set both `SMTP_USERNAME` and `SMTP_PASSWORD` for SMTP authentication, or leave
both unset for an unauthenticated relay. Credentials never appear in previews.
SMTP always uses verified TLS: `encryption = "starttls"` requires STARTTLS
(default port 587), while `encryption = "tls"` uses implicit TLS (default port
465). `smtp_port` can override these defaults. `to` accepts multiple recipients;
`subject` defaults to `Backup repository alerts`.

Exit codes for both commands (including dry runs): **0** for a completed check
with no missing repositories or failed headers (even when stale or unconfigured
folders exist), **1** for missing repositories or failed header checks, **2** for
configuration, credentials, network, email delivery, or other execution errors.

## Docker / scheduled reports

```sh
cp .env.example .env
cp config.example.toml config.toml
# Edit both files, including the [email] section in config.toml.
docker compose up -d --build
docker compose logs -f
```

No Compose override file is needed. `.env` holds the AWS/SMTP credentials and
schedule; `config.toml` holds the endpoint, repositories, and email settings.
Both local files are gitignored and excluded from the Docker build context.
Compose mounts `config.toml` read-only and refuses to create it if missing.

The container runs `report` through BusyBox cron in the foreground, with `tini`
handling signals. `CRON_SCHEDULE` defaults to `5 0 * * *` (daily at 00:05), and
`TZ` defaults to `Etc/UTC`. Reports start on the next scheduled run, not on
container startup. Cron and CLI output are forwarded to container logs. CLI
failures do not stop the scheduler. After editing `.env`, run
`docker compose up -d` to recreate the container with the new environment.

Run the CLI manually in a temporary container using the same configuration:

```sh
docker compose run --rm backup-repos-checker check
docker compose run --rm backup-repos-checker report --dry-run
docker compose run --rm backup-repos-checker report
```

The Dockerfile uses a multi-stage Rust build; the runtime image only contains
the CLI, cron, CA certificates, timezone data, and the init process.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

`src/storage.rs` defines the recursive storage interface; additional backends can
implement it without changing checks or rendering in `src/check.rs`.
