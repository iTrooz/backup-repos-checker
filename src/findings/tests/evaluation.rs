//! Assertion helpers; production Folder exposes structural operations only.

use super::*;

/// Keep scan fixtures concise while exercising the same evaluator as both commands.
pub(super) trait EvaluateFixture {
    /// Render a fixture through public evaluation and presentation.
    fn render(
        &self,
        uri: &str,
        now: DateTime<Utc>,
        days: u32,
        color: bool,
        checks: Selection,
    ) -> String;
    /// Format actionable results for assertion convenience.
    fn issues(&self, uri: &str, now: DateTime<Utc>, days: u32, checks: Selection) -> Vec<String>;
    /// Inspect the shared findings' exit-code impact.
    fn failed(&self, checks: Selection) -> bool;
    /// Select content errors from shared findings for assertion convenience.
    fn content_diagnostics(&self, uri: &str, checks: Selection) -> Vec<String>;
}

impl EvaluateFixture for Folder {
    /// Render solely from the public evaluator's findings.
    fn render(
        &self,
        uri: &str,
        now: DateTime<Utc>,
        days: u32,
        color: bool,
        checks: Selection,
    ) -> String {
        crate::check::render(self, uri, &findings(self, uri, now, days, checks), color)
    }

    /// Format only actionable findings, matching alert report selection.
    fn issues(&self, uri: &str, now: DateTime<Utc>, days: u32, checks: Selection) -> Vec<String> {
        findings(self, uri, now, days, checks)
            .iter()
            .filter(|finding| finding.is_issue())
            .map(Finding::line)
            .collect()
    }

    /// Determine failure from findings, not from repository fields.
    fn failed(&self, checks: Selection) -> bool {
        findings(self, "s3://test", Utc::now(), 7, checks)
            .iter()
            .any(|finding| finding.fatal)
    }

    /// Inspect content errors through the same shared findings, without separate evaluation logic.
    fn content_diagnostics(&self, uri: &str, checks: Selection) -> Vec<String> {
        findings(self, uri, Utc::now(), 7, checks)
            .iter()
            .filter(|finding| {
                finding.check == super::super::selection::Kind::Content && finding.is_issue()
            })
            .map(Finding::line)
            .collect()
    }
}
