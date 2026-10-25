//! Stream bucket inventories and collect repository measurements without evaluating issues.

use anyhow::Result;
use futures::{StreamExt, TryStreamExt};

use super::{Folder, Repository, selection::Selection};
use crate::{
    config::{RepositoryLocation, contains},
    storage::Storage,
};

/// Maximum in-flight entry checks, including any required prefix downloads.
const CHECK_CONCURRENCY: usize = 8;

/// Maximum offending-path samples retained per repository, not a limit on checks.
pub(super) const MAX_FAILURE_SAMPLES: usize = 10;

/// Scan once, downloading prefixes only for enabled content checks with matching rules.
pub async fn inspect(
    storage: &dyn Storage,
    repositories: &[RepositoryLocation<'_>],
    checks: Selection,
) -> Result<Folder> {
    let mut root = Folder::default();

    // Seed configured paths so missing repositories still appear in the result.
    for repository in repositories {
        root.at(repository.prefix).repository = Some(Repository {
            max_size: repository.max_size,
            silence_checks: repository.silence_checks.to_vec(),
            ..Repository::default()
        });
    }

    let entries = storage.list().await?;

    // Bound concurrent prefix downloads while preserving listing order for diagnostics.
    let mut inspected = entries
        .map(|entry| async move {
            let entry = entry?;

            // Match whole components; a bare object "repo" is not the folder "repo/".
            let repository = repositories.iter().find(|repo| {
                repo.prefix.is_empty()
                    || (contains(repo.prefix, &entry.path) && entry.path != repo.prefix)
            });

            let mut checked = false;
            let mut failure = None;

            if !entry.path.ends_with('/')
                && let Some(repository) = repository
                && checks.without(repository.silence_checks).content
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

    // Aggregate entries, propagating listing and content-read errors rather than partial results.
    while let Some((entry, repository, checked, failure)) = inspected.try_next().await? {
        let marker = entry.path.ends_with('/');
        if !marker && (repository.is_some() || checks.folders) {
            let folder = repository.map_or_else(
                || entry.path.rsplit_once('/').map_or("", |(parent, _)| parent),
                |repo| repo.prefix,
            );
            root.count_file(folder, entry.size);
        }

        if let Some(repository) = repository {
            let repo = root
                .at(repository.prefix)
                .repository
                .as_mut()
                .expect("configured repository");

            repo.exists = true;
            if !marker {
                repo.size += entry.size;
            }
            if checked {
                repo.checked_files += 1;
            }

            if let Some(failure) = failure {
                // Count every failure, retaining only a bounded sample of offending paths.
                repo.content_failures += 1;
                if repo.failure_samples.len() < MAX_FAILURE_SAMPLES {
                    repo.failure_samples.push(failure);
                }
            }

            // Markers establish existence, but their timestamps do not count for age.
            if checks.without(repository.silence_checks).age && !marker {
                repo.newest = Some(
                    repo.newest
                        .map_or(entry.modified, |old| old.max(entry.modified)),
                );
            }
        } else if checks.folders {
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
