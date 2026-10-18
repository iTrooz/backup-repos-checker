//! Repository measurements and independent evaluation of each enabled check.

use chrono::{DateTime, Utc};

use super::{
    Finding, Severity,
    selection::{Kind, Selection},
};

/// Aggregated scan measurements plus the repository's configured exclusions.
#[derive(Debug, Default)]
pub struct Repository {
    pub(super) exists: bool,
    pub(super) newest: Option<DateTime<Utc>>,
    pub(super) checked_files: usize,
    pub(super) content_failures: usize,
    pub(super) failure_samples: Vec<String>,
    pub(super) silence_checks: Vec<Kind>,
}

impl Repository {
    /// Evaluate checks independently so content errors never hide age warnings.
    pub fn findings(&self, uri: &str, cutoff: DateTime<Utc>, checks: Selection) -> Vec<Finding> {
        let checks = checks.without(&self.silence_checks);
        let mut results = Vec::new();

        if checks.folders {
            results.push(Finding::new(
                uri,
                Kind::Folders,
                if self.exists {
                    Severity::Passed
                } else {
                    Severity::Error
                },
                if self.exists { "found" } else { "not found" }.to_owned(),
                !self.exists,
            ));
        }

        // Missing repositories have no age or content measurements to evaluate.
        if !self.exists {
            return results;
        }

        if checks.age {
            let fresh = self.newest.is_some_and(|modified| modified >= cutoff);
            let message = self.newest.map_or_else(
                || "no files".to_owned(),
                |timestamp| format!("latest is {}", timestamp.format("%Y-%m-%dT%H:%M:%S")),
            );

            results.push(Finding::new(
                uri,
                Kind::Age,
                if fresh {
                    Severity::Passed
                } else {
                    Severity::Warning
                },
                message,
                false,
            ));
        }

        if checks.content {
            let failed = self.content_failures > 0;
            let (severity, message) = if failed {
                (
                    Severity::Error,
                    format!("{} files do not match pattern", self.content_failures),
                )
            } else if self.checked_files > 0 {
                (Severity::Passed, "0 files do not match pattern".to_owned())
            } else {
                (Severity::Neutral, "no files checked".to_owned())
            };

            let mut finding = Finding::new(uri, Kind::Content, severity, message, failed);
            if failed {
                finding.details = self.failure_samples.clone();
                if self.content_failures > self.failure_samples.len() {
                    finding
                        .details
                        .push("additional failures omitted".to_owned());
                }
            }

            results.push(finding);
        }

        results
    }
}
