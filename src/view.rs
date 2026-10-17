use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, TimeDelta, Utc};
use futures::TryStreamExt;

use crate::{config::contains, storage::Storage};

#[derive(Debug, Default)]
struct Repository {
    exists: bool,
    newest: Option<DateTime<Utc>>,
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

    pub fn missing(&self) -> bool {
        self.repository.as_ref().is_some_and(|repo| !repo.exists)
            || self.children.values().any(Self::missing)
    }

    fn status(&self, cutoff: DateTime<Utc>) -> Status {
        match &self.repository {
            Some(repo) if !repo.exists => Status::Missing,
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

    fn collect_issues(&self, path: &str, cutoff: DateTime<Utc>, issues: &mut Vec<String>) {
        match self.status(cutoff) {
            Status::Missing => issues.push(format!("[MISSING] {path}")),
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
            Status::Unconfigured => "31",
            Status::Group => "",
        };
        let detail = match &self.repository {
            Some(repo) if !repo.exists => "  [not found]".to_owned(),
            Some(repo) => repo.newest.map_or_else(
                || "  [no files]".to_owned(),
                |timestamp| format!("  [latest: {}]", timestamp.to_rfc3339()),
            ),
            None => String::new(),
        };
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

pub async fn inspect(storage: &dyn Storage, prefixes: &[String]) -> Result<Folder> {
    let mut root = Folder::default();
    for prefix in prefixes {
        root.at(prefix).repository = Some(Repository::default());
    }
    let mut entries = storage.list().await?;
    while let Some(entry) = entries.try_next().await? {
        // A bare object named "repo" is not the folder "repo/".
        let marker = entry.path.ends_with('/');
        if let Some(prefix) = prefixes.iter().find(|prefix| {
            prefix.is_empty() || (contains(prefix, &entry.path) && entry.path != **prefix)
        }) {
            let repo = root
                .at(prefix)
                .repository
                .as_mut()
                .expect("configured repository");
            repo.exists = true;
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
        assert!(tree.missing());
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
                .missing()
        );
    }

    #[tokio::test]
    async fn bucket_root_repository_collapses_everything() {
        let storage = FakeStorage(vec![("deep/file", Utc::now())]);
        let tree = inspect(&storage, &[String::new()]).await.unwrap();
        assert!(!tree.missing());
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
            async fn list(&self) -> Result<BoxStream<'_, Result<FileEntry>>> {
                Ok(Box::pin(stream::iter([Err(anyhow::anyhow!(
                    "listing failed"
                ))])))
            }
        }
        assert!(inspect(&Broken, &["repo".to_owned()]).await.is_err());
    }
}
