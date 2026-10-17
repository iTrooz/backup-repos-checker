//! CLI entry point for selected checks and complete email reports.

mod check;
mod config;
mod header;
mod report;
mod storage;

use std::{
    io::{self, IsTerminal, Write},
    process::ExitCode,
};

use anyhow::{Context, Result};
use check::selection::{Kind, Selection};
use chrono::Utc;
use clap::{Parser, Subcommand};

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
    /// Email red and yellow entries; requires an [email] section in config.toml.
    Report {
        /// Print the email without connecting to SMTP or sending it.
        #[arg(long)]
        dry_run: bool,
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
        let tree = check::inspect(&storage, &prefixes, checks)
            .await
            .with_context(|| format!("cannot check s3://{bucket}"))?;
        trees.push((bucket, tree));
    }
    // Only render or send after every listing succeeds: never report partial results.
    let now = Utc::now();
    let failed = trees.iter().any(|(_, tree)| tree.failed(checks));
    match cli.command {
        Command::Check { .. } => {
            let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
            let mut stdout = io::stdout().lock();
            for (bucket, tree) in trees {
                write!(
                    stdout,
                    "{}",
                    tree.render(&format!("s3://{bucket}"), now, max_age_days, color, checks)
                )?;
                for diagnostic in tree.header_diagnostics(&format!("s3://{bucket}"), checks) {
                    eprintln!("{diagnostic}");
                }
            }
        }
        Command::Report { dry_run } => {
            let issues: Vec<_> = trees
                .iter()
                .flat_map(|(bucket, tree)| {
                    tree.issues(&format!("s3://{bucket}"), now, max_age_days, checks)
                })
                .collect();
            if issues.is_empty() {
                println!("No red or yellow entries; no email sent.");
            } else {
                let email = email.expect("report configuration validated");
                let body = report::body(&issues, max_age_days);
                let message = email.message(body.clone())?;
                if dry_run {
                    // Preview readable text, not MIME-encoded content or credentials.
                    println!(
                        "From: {}\nTo: {}\nSubject: {}\n\n{body}",
                        email.from,
                        email.to.join(", "),
                        email.subject
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
