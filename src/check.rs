//! Inspect repository metadata and optional file headers, then render findings.

/// CLI check names and execution flags.
pub mod selection;

use selection::Selection;
use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, TimeDelta, Utc};
use futures::{StreamExt, TryStreamExt};

use crate::{
    config::{RepositoryLocation, contains},
    header::{CHECK_CONCURRENCY, MAX_FAILURE_SAMPLES},
    storage::Storage,
};

#[derive(Debug, Default)]
struct Repository {
    exists: bool,
    newest: Option<DateTime<Utc>>,
    checked_files: usize,
    header_failures: usize,
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
    InvalidHeaders,
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
            (checks.folders && !repo.exists) || (checks.content && repo.header_failures > 0)
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
            Some(repo) if checks.content && repo.header_failures > 0 => Status::InvalidHeaders,
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

    fn header_failure(&self, path: &str) -> Option<String> {
        let repo = self.repository.as_ref()?;
        (repo.header_failures > 0).then(|| {
            format!(
                "[HEADER CHECK FAILED] {path} ({}/{} files failed): {}{}",
                repo.header_failures,
                repo.checked_files,
                repo.failure_samples.join("; "),
                if repo.header_failures > repo.failure_samples.len() {
                    "; additional failures omitted"
                } else {
                    ""
                },
            )
        })
    }

    /// Return bounded content diagnostics only when content checks are enabled.
    pub fn header_diagnostics(&self, path: &str, checks: Selection) -> Vec<String> {
        if !checks.content {
            return Vec::new();
        }
        let mut issues: Vec<_> = self.header_failure(path).into_iter().collect();
        for (name, child) in &self.children {
            issues.extend(child.header_diagnostics(&format!("{path}/{name}"), checks));
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
            Status::InvalidHeaders => {
                let repo = self
                    .repository
                    .as_ref()
                    .expect("repository with invalid headers");
                issues.push(self.header_failure(path).expect("invalid headers"));
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

    /// Render a collapsed repository line without details from disabled checks.
    fn render_into(
        &self,
        name: &str,
        indent: &str,
        last: Option<bool>,
        options: RenderOptions,
        output: &mut String,
    ) {
        let style = match self.status(options.cutoff, options.checks) {
            Status::Fresh => "32",
            Status::Stale => "33",
            Status::Missing => "31;4",
            Status::InvalidHeaders => "31",
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
                "  [header failures: {}/{}]",
                repo.header_failures, repo.checked_files
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
        if self.repository.is_some() {
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

/// List every repository, downloading headers only when content is selected.
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
    // Bound concurrent header downloads while preserving listing order for diagnostics.
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
            // Read headers only for opted-in repositories, never for directory markers.
            if checks.content
                && !entry.path.ends_with('/')
                && let Some(repository) = repository
                && let Some(header_check) = repository.header_check
            {
                checked = true;
                // Rules use repository-relative paths; bucket-root repos already have those.
                let relative = if repository.prefix.is_empty() {
                    entry.path.as_str()
                } else {
                    &entry.path[repository.prefix.len() + 1..]
                };
                failure = header_check
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
                repo.header_failures += 1;
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

/// Freshness and folder-tree behavior under the default selection.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::FileEntry;
    use async_trait::async_trait;
    use futures::{stream, stream::BoxStream};

    struct FakeStorage(Vec<(&'static str, DateTime<Utc>)>);

    #[async_trait]
    impl Storage for FakeStorage {
        async fn read_prefix(&self, _path: &str, _length: usize) -> Result<Vec<u8>> {
            panic!("freshness-only checks must not download objects");
        }
        async fn list(&self) -> Result<BoxStream<'_, Result<FileEntry>>> {
            Ok(Box::pin(stream::iter(self.0.iter().map(
                |(path, modified)| {
                    Ok(FileEntry {
                        path: (*path).to_owned(),
                        modified: *modified,
                    })
                },
            ))))
        }
    }

    /// Adapt freshness fixtures to the full default selection.
    async fn inspect(storage: &dyn Storage, prefixes: &[String]) -> Result<Folder> {
        let repositories: Vec<_> = prefixes
            .iter()
            .map(|prefix| RepositoryLocation {
                prefix,
                header_check: None,
            })
            .collect();
        super::inspect(storage, &repositories, Selection::default()).await
    }

    #[tokio::test]
    async fn recursive_freshness_and_tree_colors() {
        let now = Utc::now();
        let storage = FakeStorage(vec![
            ("backups/fresh/deep/file", now - TimeDelta::days(7)),
            ("backups/fresh/old", now - TimeDelta::days(20)),
            ("backups/stale/file", now - TimeDelta::days(8)),
            ("backups/empty/", now),
            ("untracked/child/file", now),
        ]);
        let prefixes =
            ["backups/fresh", "backups/stale", "backups/empty", "missing"].map(str::to_owned);
        let tree = inspect(&storage, &prefixes).await.unwrap();
        let rendered = tree.render("s3://test", now, 7, true, Selection::default());
        assert!(rendered.contains("\x1b[32mfresh"));
        assert!(rendered.contains("\x1b[33mstale"));
        assert!(rendered.contains("\x1b[33mempty  [no files]"));
        assert!(rendered.contains("\x1b[31;4mmissing  [not found]"));
        assert!(rendered.contains("\x1b[31muntracked"));
        assert!(!rendered.contains("deep"));
        assert!(!rendered.contains("\x1b[31mbackups"));
        assert!(tree.failed(Selection::default()));
        assert!(
            !tree
                .render("s3://test", now, 7, false, Selection::default())
                .contains('\x1b')
        );
        let issues = tree.issues("s3://test", now, 7, Selection::default());
        assert_eq!(issues.len(), 5);
        assert!(
            issues
                .iter()
                .any(|issue| issue == "[MISSING] s3://test/missing")
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue == "[UNCONFIGURED] s3://test/untracked/child")
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.starts_with("[STALE] s3://test/backups/empty (no files)"))
        );
        assert!(
            !issues
                .iter()
                .any(|issue| issue.contains("fresh") || issue.contains("deep"))
        );
    }

    #[tokio::test]
    async fn exact_object_and_similar_prefix_do_not_establish_existence() {
        let storage = FakeStorage(vec![("repo", Utc::now()), ("repository/file", Utc::now())]);
        assert!(
            inspect(&storage, &["repo".to_owned()])
                .await
                .unwrap()
                .failed(Selection::default())
        );
    }

    #[tokio::test]
    async fn listing_errors_propagate() {
        struct Broken;
        #[async_trait]
        impl Storage for Broken {
            async fn read_prefix(&self, _path: &str, _length: usize) -> Result<Vec<u8>> {
                unreachable!()
            }
            async fn list(&self) -> Result<BoxStream<'_, Result<FileEntry>>> {
                Ok(Box::pin(stream::iter([Err(anyhow::anyhow!(
                    "listing failed"
                ))])))
            }
        }
        assert!(inspect(&Broken, &["repo".to_owned()]).await.is_err());
    }
}

/// Content-check tests kept separate to keep production modules small.
#[cfg(test)]
#[path = "check/header_tests.rs"]
mod header_tests;
