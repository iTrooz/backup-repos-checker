//! Shared, structured check outcomes for tree rendering, email, and exit codes.

use chrono::{DateTime, TimeDelta, Utc};

use super::{
    Folder,
    selection::{Kind, Selection},
};

/// Display priority; neutral outcomes never claim a check passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Neutral,
    Passed,
    Warning,
    Error,
}

/// One evaluated check result; message formatting does not determine its behavior.
#[derive(Debug)]
pub struct Finding {
    pub uri: String,
    pub check: Kind,
    pub severity: Severity,
    pub message: String,
    pub details: Vec<String>,
    pub fatal: bool,
}

impl Finding {
    /// Construct a result with no diagnostic samples.
    pub(super) fn new(
        uri: &str,
        check: Kind,
        severity: Severity,
        message: String,
        fatal: bool,
    ) -> Self {
        Self {
            uri: uri.to_owned(),
            check,
            severity,
            message,
            details: Vec::new(),
            fatal,
        }
    }

    /// Whether this result should trigger an alert rather than a healthy confirmation.
    pub fn is_issue(&self) -> bool {
        self.severity >= Severity::Warning
    }

    /// Format one report line, preserving bounded diagnostic details when present.
    pub fn line(&self) -> String {
        let mut line = format!("{}: {}: {}", self.uri, self.check.name(), self.message);
        if !self.details.is_empty() {
            line.push_str(&format!(": {}", self.details.join("; ")));
        }
        line
    }
}

/// Evaluate every configured repository and unconfigured folder once for both commands.
pub fn findings(
    tree: &Folder,
    uri: &str,
    now: DateTime<Utc>,
    days: u32,
    checks: Selection,
) -> Vec<Finding> {
    let mut results = Vec::new();
    collect(
        tree,
        uri,
        now - TimeDelta::days(days.into()),
        checks,
        &mut results,
    );
    results
}

/// Traverse structure only; repository-specific check conditions live on Repository.
fn collect(
    tree: &Folder,
    uri: &str,
    cutoff: DateTime<Utc>,
    checks: Selection,
    results: &mut Vec<Finding>,
) {
    if let Some(repository) = &tree.repository {
        results.extend(repository.findings(uri, cutoff, checks));
        return;
    }
    if checks.folders && !tree.has_repository() {
        results.push(Finding::new(
            uri,
            Kind::Folders,
            Severity::Error,
            "unconfigured".to_owned(),
            false,
        ));
    }
    // Report descendants even when their parent will be collapsed in the displayed tree.
    for (name, child) in &tree.children {
        collect(child, &format!("{uri}/{name}"), cutoff, checks, results);
    }
}
