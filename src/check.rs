//! Inspect repository metadata and optional file content, then render findings.

/// Repository-specific byte-pattern validation.
pub mod content;

/// CLI check names and execution flags.
pub mod selection;

use selection::Selection;
use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, TimeDelta, Utc};
use futures::{StreamExt, TryStreamExt};

use crate::{
    config::{RepositoryLocation, contains},
    storage::Storage,
};

// Bound scan work and retained diagnostics independently of the content validator.
const CHECK_CONCURRENCY: usize = 8;
const MAX_FAILURE_SAMPLES: usize = 10;

#[derive(Debug, Default)]
struct Repository {
    exists: bool,
    newest: Option<DateTime<Utc>>,
    checked_files: usize,
    content_failures: usize,
    failure_samples: Vec<String>,
}

/// One folder-tree node containing repository findings and named child folders.
#[derive(Debug, Default)]
pub struct Folder {
    repository: Option<Repository>,
    children: BTreeMap<String, Folder>,
}

/// Run-wide rendering options passed unchanged through tree traversal.
#[derive(Clone, Copy)]
struct RenderOptions {
    cutoff: DateTime<Utc>,
    color: bool,
    checks: Selection,
}

#[derive(Clone, Copy)]
enum Status {
    Fresh,
    Stale,
    Missing,
    InvalidContent,
    Unconfigured,
    Group,
}

impl Folder {
    /// Create missing ancestors and return the node at the requested path.
    fn at(&mut self, path: &str) -> &mut Self {
        let mut folder = self;
        for component in path.split('/').filter(|part| !part.is_empty()) {
            folder = folder.children.entry(component.to_owned()).or_default();
        }
        folder
    }

    fn has_repository(&self) -> bool {
        self.repository.is_some() || self.children.values().any(Self::has_repository)
    }

    /// Only enabled folder and content failures affect the health exit code.
    pub fn failed(&self, checks: Selection) -> bool {
        self.repository.as_ref().is_some_and(|repo| {
            (checks.folders && !repo.exists) || (checks.content && repo.content_failures > 0)
        }) || self.children.values().any(|child| child.failed(checks))
    }

    /// Keep skipped checks neutral rather than claiming they passed or failed.
    fn status(&self, cutoff: DateTime<Utc>, checks: Selection) -> Status {
        match &self.repository {
            Some(repo) if !repo.exists => {
                if checks.folders {
                    Status::Missing
                } else {
                    Status::Group
                }
            }
            Some(repo) if checks.content && repo.content_failures > 0 => Status::InvalidContent,
            Some(repo) if checks.age => {
                if repo.newest.is_some_and(|modified| modified >= cutoff) {
                    Status::Fresh
                } else {
                    Status::Stale
                }
            }
            Some(repo) if checks.folders || (checks.content && repo.checked_files > 0) => {
                Status::Fresh
            }
            Some(_) => Status::Group,
            None if checks.folders && !self.has_repository() => Status::Unconfigured,
            None => Status::Group,
        }
    }

    /// Evaluate stored findings using the run-wide check selection.
    pub fn issues(
        &self,
        path: &str,
        now: DateTime<Utc>,
        days: u32,
        checks: Selection,
    ) -> Vec<String> {
        let mut issues = Vec::new();
        self.collect_issues(
            path,
            now - TimeDelta::days(days.into()),
            checks,
            &mut issues,
        );
        issues
    }

    /// Format aggregated content failures with bounded offending-path samples.
    fn content_failure(&self, path: &str) -> Option<String> {
        let repo = self.repository.as_ref()?;
        (repo.content_failures > 0).then(|| {
            format!(
                "[CONTENT CHECK FAILED] {path} ({}/{} files failed): {}{}",
                repo.content_failures,
                repo.checked_files,
                repo.failure_samples.join("; "),
                if repo.content_failures > repo.failure_samples.len() {
                    "; additional failures omitted"
                } else {
                    ""
                },
            )
        })
    }

    /// Return bounded content diagnostics only when content checks are enabled.
    pub fn content_diagnostics(&self, path: &str, checks: Selection) -> Vec<String> {
        if !checks.content {
            return Vec::new();
        }
        let mut issues: Vec<_> = self.content_failure(path).into_iter().collect();
        for (name, child) in &self.children {
            issues.extend(child.content_diagnostics(&format!("{path}/{name}"), checks));
        }
        issues
    }

    /// Collect only enabled findings for reports and diagnostics.
    fn collect_issues(
        &self,
        path: &str,
        cutoff: DateTime<Utc>,
        checks: Selection,
        issues: &mut Vec<String>,
    ) {
        match self.status(cutoff, checks) {
            Status::Missing => issues.push(format!("[MISSING] {path}")),
            Status::InvalidContent => {
                let repo = self
                    .repository
                    .as_ref()
                    .expect("repository with invalid content");
                issues.push(self.content_failure(path).expect("invalid content"));
                if checks.age && repo.newest.is_none_or(|modified| modified < cutoff) {
                    issues.push(format!("[STALE] {path}"));
                }
            }
            Status::Stale => {
                let latest = self
                    .repository
                    .as_ref()
                    .and_then(|repo| repo.newest)
                    .map_or_else(
                        || "no files".to_owned(),
                        |timestamp| format!("latest: {}", timestamp.to_rfc3339()),
                    );
                issues.push(format!("[STALE] {path} ({latest})"));
            }
            Status::Unconfigured => issues.push(format!("[UNCONFIGURED] {path}")),
            Status::Fresh | Status::Group => {}
        }
        if self.repository.is_none() {
            for (name, child) in &self.children {
                child.collect_issues(&format!("{path}/{name}"), cutoff, checks, issues);
            }
        }
    }

    /// Render results without storing run-wide selection or formatting on tree nodes.
    pub fn render(
        &self,
        name: &str,
        now: DateTime<Utc>,
        days: u32,
        color: bool,
        checks: Selection,
    ) -> String {
        let mut output = String::new();
        self.render_into(
            name,
            "",
            None,
            RenderOptions {
                cutoff: now - TimeDelta::days(days.into()),
                color,
                checks,
            },
            &mut output,
        );
        output
    }

    /// Render repositories and unconfigured folders as single collapsed lines.
    fn render_into(
        &self,
        name: &str,
        indent: &str,
        last: Option<bool>,
        options: RenderOptions,
        output: &mut String,
    ) {
        let status = self.status(options.cutoff, options.checks);
        let style = match status {
            Status::Fresh => "32",
            Status::Stale => "33",
            Status::Missing => "31;4",
            Status::InvalidContent => "31",
            Status::Unconfigured => "31",
            Status::Group => "",
        };
        let mut detail = match &self.repository {
            Some(repo) if !repo.exists => if options.checks.folders {
                "  [not found]"
            } else {
                "  [not checked]"
            }
            .to_owned(),
            Some(repo) if options.checks.age => repo.newest.map_or_else(
                || "  [no files]".to_owned(),
                |timestamp| format!("  [latest: {}]", timestamp.to_rfc3339()),
            ),
            _ => String::new(),
        };
        if let Some(repo) = &self.repository
            && options.checks.content
            && repo.checked_files > 0
        {
            detail.push_str(&format!(
                "  [content failures: {}/{}]",
                repo.content_failures, repo.checked_files
            ));
        }
        output.push_str(indent);
        if let Some(last) = last {
            output.push_str(if last { "└── " } else { "├── " });
        }
        if options.color && !style.is_empty() {
            output.push_str(&format!("\x1b[{style}m{name}{detail}\x1b[0m\n"));
        } else {
            output.push_str(&format!("{name}{detail}\n"));
        }
        // Collapse display only: descendants remain available to scans and reports.
        if self.repository.is_some() || matches!(status, Status::Unconfigured) {
            return;
        }
        let child_indent = format!(
            "{indent}{}",
            match last {
                Some(true) => "    ",
                Some(false) => "│   ",
                None => "",
            }
        );
        for (index, (name, child)) in self.children.iter().enumerate() {
            child.render_into(
                name,
                &child_indent,
                Some(index + 1 == self.children.len()),
                options,
                output,
            );
        }
    }
}

/// List every repository, downloading prefixes only when content is selected.
pub async fn inspect(
    storage: &dyn Storage,
    repositories: &[RepositoryLocation<'_>],
    checks: Selection,
) -> Result<Folder> {
    // Seed configured paths so missing repositories still appear in the result.
    let mut root = Folder::default();
    for repository in repositories {
        root.at(repository.prefix).repository = Some(Repository::default());
    }
    let entries = storage.list().await?;
    // Bound concurrent prefix downloads while preserving listing order for diagnostics.
    let mut inspected = entries
        .map(|entry| async move {
            let entry = entry?;
            // Match whole path components; a bare object "repo" is not the folder "repo/".
            // An empty prefix represents a repository covering the entire bucket.
            let repository = repositories.iter().find(|repo| {
                repo.prefix.is_empty()
                    || (contains(repo.prefix, &entry.path) && entry.path != repo.prefix)
            });
            let mut checked = false;
            let mut failure = None;
            // Read content only for opted-in repositories, never for directory markers.
            if checks.content
                && !entry.path.ends_with('/')
                && let Some(repository) = repository
                && let Some(content_check) = repository.content_check
            {
                checked = true;
                // Rules use repository-relative paths; bucket-root repos already have those.
                let relative = if repository.prefix.is_empty() {
                    entry.path.as_str()
                } else {
                    &entry.path[repository.prefix.len() + 1..]
                };
                failure = content_check
                    .check(storage, &entry.path, relative)
                    .await?
                    .map(|reason| format!("{relative:?}: {reason}"));
            }
            Ok::<_, anyhow::Error>((entry, repository, checked, failure))
        })
        .buffered(CHECK_CONCURRENCY);
    // Merge completed checks into collapsed repository nodes, propagating storage errors.
    while let Some((entry, repository, checked, failure)) = inspected.try_next().await? {
        let marker = entry.path.ends_with('/');
        if let Some(repository) = repository {
            let repo = root
                .at(repository.prefix)
                .repository
                .as_mut()
                .expect("configured repository");
            repo.exists = true;
            if checked {
                repo.checked_files += 1;
            }
            if let Some(failure) = failure {
                // Count every failure, but retain only a bounded sample of offending paths.
                repo.content_failures += 1;
                if repo.failure_samples.len() < MAX_FAILURE_SAMPLES {
                    repo.failure_samples.push(failure);
                }
            }
            // Directory-marker timestamps establish existence, not backup freshness.
            if checks.age && !marker {
                repo.newest = Some(
                    repo.newest
                        .map_or(entry.modified, |old| old.max(entry.modified)),
                );
            }
        } else if checks.folders {
            // Expose unconfigured folders, excluding object filenames from the tree.
            let folder = if marker {
                entry.path.trim_end_matches('/')
            } else {
                entry.path.rsplit_once('/').map_or("", |(parent, _)| parent)
            };
            root.at(folder);
        }
    }
    Ok(root)
}

/// Scan behavior grouped by age, folders, content, and check selection.
#[cfg(test)]
mod tests;
