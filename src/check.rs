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

#[derive(Debug, Default)]
pub struct Folder {
    repository: Option<Repository>,
    children: BTreeMap<String, Folder>,
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

    pub fn failed(&self) -> bool {
        self.repository
            .as_ref()
            .is_some_and(|repo| !repo.exists || repo.header_failures > 0)
            || self.children.values().any(Self::failed)
    }

    fn status(&self, cutoff: DateTime<Utc>) -> Status {
        match &self.repository {
            Some(repo) if !repo.exists => Status::Missing,
            Some(repo) if repo.header_failures > 0 => Status::InvalidHeaders,
            Some(repo) if repo.newest.is_some_and(|modified| modified >= cutoff) => Status::Fresh,
            Some(_) => Status::Stale,
            None if !self.has_repository() => Status::Unconfigured,
            None => Status::Group,
        }
    }

    pub fn issues(&self, path: &str, now: DateTime<Utc>, days: u32) -> Vec<String> {
        let mut issues = Vec::new();
        self.collect_issues(path, now - TimeDelta::days(days.into()), &mut issues);
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

    pub fn header_diagnostics(&self, path: &str) -> Vec<String> {
        let mut issues: Vec<_> = self.header_failure(path).into_iter().collect();
        for (name, child) in &self.children {
            issues.extend(child.header_diagnostics(&format!("{path}/{name}")));
        }
        issues
    }

    fn collect_issues(&self, path: &str, cutoff: DateTime<Utc>, issues: &mut Vec<String>) {
        match self.status(cutoff) {
            Status::Missing => issues.push(format!("[MISSING] {path}")),
            Status::InvalidHeaders => {
                let repo = self
                    .repository
                    .as_ref()
                    .expect("repository with invalid headers");
                issues.push(self.header_failure(path).expect("invalid headers"));
                if repo.newest.is_none_or(|modified| modified < cutoff) {
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
                child.collect_issues(&format!("{path}/{name}"), cutoff, issues);
            }
        }
    }

    pub fn render(&self, name: &str, now: DateTime<Utc>, days: u32, color: bool) -> String {
        let mut output = String::new();
        self.render_into(
            name,
            "",
            None,
            now - TimeDelta::days(days.into()),
            color,
            &mut output,
        );
        output
    }

    fn render_into(
        &self,
        name: &str,
        indent: &str,
        last: Option<bool>,
        cutoff: DateTime<Utc>,
        color: bool,
        output: &mut String,
    ) {
        let style = match self.status(cutoff) {
            Status::Fresh => "32",
            Status::Stale => "33",
            Status::Missing => "31;4",
            Status::InvalidHeaders => "31",
            Status::Unconfigured => "31",
            Status::Group => "",
        };
        let mut detail = match &self.repository {
            Some(repo) if !repo.exists => "  [not found]".to_owned(),
            Some(repo) => repo.newest.map_or_else(
                || "  [no files]".to_owned(),
                |timestamp| format!("  [latest: {}]", timestamp.to_rfc3339()),
            ),
            None => String::new(),
        };
        if let Some(repo) = &self.repository
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
        if color && !style.is_empty() {
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
                cutoff,
                color,
                output,
            );
        }
    }
}

pub async fn inspect(
    storage: &dyn Storage,
    repositories: &[RepositoryLocation<'_>],
) -> Result<Folder> {
    let mut root = Folder::default();
    for repository in repositories {
        root.at(repository.prefix).repository = Some(Repository::default());
    }
    let entries = storage.list().await?;
    let mut checks = entries
        .map(|entry| async move {
            let entry = entry?;
            let repository = repositories.iter().find(|repo| {
                repo.prefix.is_empty()
                    || (contains(repo.prefix, &entry.path) && entry.path != repo.prefix)
            });
            let mut checked = false;
            let mut failure = None;
            if !entry.path.ends_with('/')
                && let Some(repository) = repository
                && let Some(header_check) = repository.header_check
            {
                checked = true;
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
    while let Some((entry, repository, checked, failure)) = checks.try_next().await? {
        // A bare object named "repo" is not the folder "repo/".
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
                repo.header_failures += 1;
                if repo.failure_samples.len() < MAX_FAILURE_SAMPLES {
                    repo.failure_samples.push(failure);
                }
            }
            if !marker {
                repo.newest = Some(
                    repo.newest
                        .map_or(entry.modified, |old| old.max(entry.modified)),
                );
            }
        } else {
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

    async fn inspect(storage: &dyn Storage, prefixes: &[String]) -> Result<Folder> {
        let repositories: Vec<_> = prefixes
            .iter()
            .map(|prefix| RepositoryLocation {
                prefix,
                header_check: None,
            })
            .collect();
        super::inspect(storage, &repositories).await
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
        let rendered = tree.render("s3://test", now, 7, true);
        assert!(rendered.contains("\x1b[32mfresh"));
        assert!(rendered.contains("\x1b[33mstale"));
        assert!(rendered.contains("\x1b[33mempty  [no files]"));
        assert!(rendered.contains("\x1b[31;4mmissing  [not found]"));
        assert!(rendered.contains("\x1b[31muntracked"));
        assert!(!rendered.contains("deep"));
        assert!(!rendered.contains("\x1b[31mbackups"));
        assert!(tree.failed());
        assert!(!tree.render("s3://test", now, 7, false).contains('\x1b'));
        let issues = tree.issues("s3://test", now, 7);
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
                .failed()
        );
    }

    #[tokio::test]
    async fn bucket_root_repository_collapses_everything() {
        let storage = FakeStorage(vec![("deep/file", Utc::now())]);
        let tree = inspect(&storage, &[String::new()]).await.unwrap();
        assert!(!tree.failed());
        assert!(tree.issues("s3://test", Utc::now(), 7).is_empty());
        assert_eq!(
            tree.render("s3://test", Utc::now(), 7, false)
                .lines()
                .count(),
            1
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

    struct HeaderStorage {
        files: BTreeMap<String, Vec<u8>>,
        reads: std::sync::atomic::AtomicUsize,
        fail_reads: bool,
    }

    #[async_trait]
    impl Storage for HeaderStorage {
        async fn list(&self) -> Result<BoxStream<'_, Result<FileEntry>>> {
            Ok(Box::pin(stream::iter(self.files.keys().map(|path| {
                Ok(FileEntry {
                    path: path.clone(),
                    modified: Utc::now(),
                })
            }))))
        }

        async fn read_prefix(&self, path: &str, length: usize) -> Result<Vec<u8>> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_reads {
                anyhow::bail!("access denied");
            }
            assert_eq!(length, crate::header::HEADER_BYTES);
            Ok(self.files[path].iter().copied().take(length).collect())
        }
    }

    #[tokio::test]
    async fn header_checks_are_recursive_and_fail_closed() {
        let storage = HeaderStorage {
            files: [
                ("repo/root.gz", vec![0x1f, 0x8b, 0x08]),
                ("repo/deep/bad.gz", b"wrong".to_vec()),
                ("repo/empty.gz", vec![]),
                ("repo/unmatched.txt", b"text".to_vec()),
                ("repo/marker/", vec![]),
                ("other/file.gz", b"ignored".to_vec()),
            ]
            .into_iter()
            .map(|(path, bytes)| (path.to_owned(), bytes))
            .collect(),
            reads: 0.into(),
            fail_reads: false,
        };
        let header_check =
            toml::from_str("[[rules]]\nglob = '**/*.gz'\npattern = '^\\x1f\\x8b\\x08'").unwrap();
        let repositories = [RepositoryLocation {
            prefix: "repo",
            header_check: Some(&header_check),
        }];
        let tree = super::inspect(&storage, &repositories).await.unwrap();
        assert!(tree.failed());
        assert_eq!(storage.reads.load(std::sync::atomic::Ordering::SeqCst), 3);
        let rendered = tree.render("s3://test", Utc::now(), 7, true);
        assert!(rendered.contains("\x1b[31mrepo"));
        assert!(rendered.contains("[header failures: 3/4]"));
        assert!(!rendered.contains("bad.gz"));
        let issues = tree.issues("s3://test", Utc::now(), 7).join("\n");
        assert!(issues.contains("[HEADER CHECK FAILED] s3://test/repo"));
        assert!(issues.contains("deep/bad.gz"));
        assert!(issues.contains("no matching header rule"));
        assert!(!issues.contains("root.gz"));
        assert!(!issues.contains("[STALE]"));
    }

    #[tokio::test]
    async fn checks_every_file_but_bounds_failure_details() {
        let storage = HeaderStorage {
            files: (0..27)
                .map(|i| (format!("deep/file{i}"), b"bad".to_vec()))
                .collect(),
            reads: 0.into(),
            fail_reads: false,
        };
        let header_check = toml::from_str("[[rules]]\nglob = '**'\npattern = '^GOOD'").unwrap();
        let repositories = [RepositoryLocation {
            prefix: "",
            header_check: Some(&header_check),
        }];
        let tree = super::inspect(&storage, &repositories).await.unwrap();
        assert_eq!(storage.reads.load(std::sync::atomic::Ordering::SeqCst), 27);
        let repo = tree.repository.as_ref().unwrap();
        assert_eq!(repo.header_failures, 27);
        assert_eq!(repo.failure_samples.len(), MAX_FAILURE_SAMPLES);
        assert!(tree.issues("s3://test", Utc::now(), 7)[0].contains("additional failures omitted"));
    }

    #[tokio::test]
    async fn header_access_failures_are_execution_errors() {
        let storage = HeaderStorage {
            files: [("repo/file".to_owned(), vec![])].into(),
            reads: 0.into(),
            fail_reads: true,
        };
        let header_check = toml::from_str("[[rules]]\nglob = '**'\npattern = '^GOOD'").unwrap();
        let repositories = [RepositoryLocation {
            prefix: "repo",
            header_check: Some(&header_check),
        }];
        assert!(super::inspect(&storage, &repositories).await.is_err());
    }
}
