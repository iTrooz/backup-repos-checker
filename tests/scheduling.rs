//! Verify cron generation without writing system paths or starting a daemon.

use std::process::{Command, Output};

/// Run the entrypoint with temporary config/crontab paths and no daemon startup.
fn schedule(normal: Option<&str>, healthy: Option<&str>) -> (Output, String) {
    let directory = tempfile::tempdir_in("/tmp/opencode").unwrap();
    let config = directory.path().join("config.toml");
    let crontab = directory.path().join("crontab");
    std::fs::write(&config, "").unwrap();
    let script = include_str!("../entrypoint.sh")
        .replace("/config/config.toml", config.to_str().unwrap())
        .replace("/etc/crontabs/root", crontab.to_str().unwrap())
        .replace("exec crond -f -d 8", "exit 0");
    let mut command = Command::new("sh");
    command
        .args(["-c", &script])
        .env_remove("CRON_SCHEDULE")
        .env_remove("HEALTHY_CRON_SCHEDULE");
    if let Some(normal) = normal {
        command.env("CRON_SCHEDULE", normal);
    }
    if let Some(healthy) = healthy {
        command.env("HEALTHY_CRON_SCHEDULE", healthy);
    }
    let output = command.output().unwrap();
    let contents = std::fs::read_to_string(crontab).unwrap_or_default();
    (output, contents)
}

/// Defaults partition the month so the 1st replaces, rather than duplicates, a report.
#[test]
fn monthly_healthy_schedule_does_not_overlap_daily_reports() {
    let (output, crontab) = schedule(None, None);
    assert!(output.status.success(), "{output:?}");
    let lines: Vec<_> = crontab.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("5 0 2-31 * * "));
    assert!(!lines[0].contains("--send-even-if-healthy"));
    assert!(lines[1].starts_with("5 0 1 * * "));
    assert!(lines[1].contains("report --send-even-if-healthy"));
}

/// Both schedules remain configurable and reject malformed multi-line input.
#[test]
fn validates_both_custom_schedules() {
    let (output, crontab) = schedule(Some("0 8 2-31 * *"), Some("0 8 1 * *"));
    assert!(output.status.success(), "{output:?}");
    assert!(crontab.starts_with("0 8 2-31 * * "));
    assert!(crontab.lines().nth(1).unwrap().starts_with("0 8 1 * * "));
    for (normal, healthy) in [
        (Some("invalid"), None),
        (None, Some("0 8 1 * *\n0 9 * * *")),
    ] {
        let (output, crontab) = schedule(normal, healthy);
        assert_eq!(output.status.code(), Some(2));
        assert!(crontab.is_empty());
    }
}
