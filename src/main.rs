//! CLI entry point for selected checks and complete email reports.

mod check;
mod config;
mod findings;
mod report;
mod storage;

use std::{
    io::{self, IsTerminal, Write},
    process::ExitCode,
};

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use findings::selection::{Kind, Selection};

#[derive(Parser)]
#[command(version, about = "Check backup repository activity in S3")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Display configured repositories and unconfigured folders using ./config.toml.
    Check {
        /// Comma-separated checks to run: folders, age, content.
        #[arg(
            long,
            value_enum,
            value_delimiter = ',',
            default_value = "folders,age,content"
        )]
        checks: Vec<Kind>,
    },
    /// Email findings, optionally including healthy results; requires [email] configuration.
    Report {
        /// Print the email without connecting to SMTP or sending it.
        #[arg(long)]
        dry_run: bool,
        /// Send a confirmation email even when all repositories are healthy.
        #[arg(long)]
        send_even_if_healthy: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(true) => ExitCode::from(1),
        Ok(false) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}

/// Run the selected checks, keeping reports on the full default selection.
async fn run(cli: Cli) -> Result<bool> {
    let checks = match &cli.command {
        Command::Check { checks } => Selection::from_kinds(checks),
        Command::Report { .. } => Selection::default(),
    };
    let text = std::fs::read_to_string("config.toml").context("cannot read ./config.toml")?;
    let config = config::Config::parse(&text)?;
    let max_age_days = config.checks.age.max_age_days;
    let email = if matches!(cli.command, Command::Report { .. }) {
        let email = config
            .email
            .as_ref()
            .context("report requires an [email] section in config.toml")?;
        email.validate()?;
        Some(email)
    } else {
        None
    };
    let mut trees = Vec::new();
    for (bucket, prefixes) in config.buckets()? {
        let storage = storage::S3Storage::new(&config, &bucket)?;
        let tree = findings::inspect(&storage, &prefixes, checks)
            .await
            .with_context(|| format!("cannot check s3://{bucket}"))?;
        trees.push((bucket, tree));
    }
    // Only render or send after every listing succeeds: never report partial results.
    let now = Utc::now();
    // Evaluate once: both commands consume the same structured check results.
    let findings: Vec<_> = trees
        .iter()
        .flat_map(|(bucket, tree)| {
            findings::findings(tree, &format!("s3://{bucket}"), now, max_age_days, checks)
        })
        .collect();
    let failed = findings.iter().any(|finding| finding.fatal);
    match cli.command {
        Command::Check { .. } => {
            let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
            let mut stdout = io::stdout().lock();
            for (bucket, tree) in trees {
                write!(
                    stdout,
                    "{}",
                    check::render(&tree, &format!("s3://{bucket}"), &findings, color)
                )?;
            }
            // Preserve offending-file diagnostics, formatting the same findings rather than rechecking.
            for finding in findings
                .iter()
                .filter(|finding| !finding.details.is_empty())
            {
                eprintln!("{}", finding.line());
            }
        }
        Command::Report {
            dry_run,
            send_even_if_healthy,
        } => {
            let issues: Vec<_> = findings
                .iter()
                .filter(|finding| finding.is_issue())
                .map(findings::Finding::line)
                .collect();
            if issues.is_empty() && !send_even_if_healthy {
                println!("No red or yellow entries; no email sent.");
            } else {
                let email = email.expect("report configuration validated");
                // Reuse this run's complete findings for either an alert or a healthy confirmation.
                let healthy = issues.is_empty();
                let subject = if healthy {
                    "Backup repositories healthy"
                } else {
                    &email.subject
                };
                let body = if healthy {
                    let lines = findings
                        .iter()
                        .map(findings::Finding::line)
                        .collect::<Vec<_>>()
                        .join("\n");
                    report::healthy_body(&lines, max_age_days)
                } else {
                    report::body(&issues, max_age_days)
                };
                let message = email.message_with_subject(body.clone(), subject)?;
                if dry_run {
                    // Preview readable text, not MIME-encoded content or credentials.
                    println!(
                        "From: {}\nTo: {}\nSubject: {}\n\n{body}",
                        email.from,
                        email.to.join(", "),
                        subject
                    );
                } else {
                    email.send(message).await?;
                    println!("Report emailed ({} problematic entries).", issues.len());
                }
            }
        }
    }
    Ok(failed)
}
