use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub s3_endpoint: String,
    pub s3_region: String,
    #[serde(default = "default_max_age")]
    pub max_age_days: u32,
    pub repositories: Vec<String>,
    pub email: Option<crate::report::EmailConfig>,
}

fn default_max_age() -> u32 {
    7
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text).context("invalid config.toml")?;
        let endpoint = url::Url::parse(&config.s3_endpoint).context("invalid S3 endpoint")?;
        ensure!(
            matches!(endpoint.scheme(), "http" | "https") && endpoint.host_str().is_some(),
            "S3 endpoint must be an HTTP(S) URL"
        );
        ensure!(
            !config.s3_region.trim().is_empty(),
            "S3 region cannot be empty"
        );
        ensure!(config.max_age_days > 0, "max_age_days must be positive");
        ensure!(
            !config.repositories.is_empty(),
            "configure at least one repository"
        );
        config.buckets()?;
        Ok(config)
    }

    /// Bucket names mapped to normalized repository prefixes (empty means bucket root).
    pub fn buckets(&self) -> Result<BTreeMap<String, Vec<String>>> {
        let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for repository in &self.repositories {
            let Some(rest) = repository.strip_prefix("s3://") else {
                bail!("repository must start with s3://: {repository}");
            };
            let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
            ensure!(
                !bucket.is_empty()
                    && bucket
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.'),
                "invalid bucket in {repository}"
            );
            let prefix = prefix.trim_end_matches('/');
            ensure!(
                prefix.is_empty()
                    || prefix
                        .split('/')
                        .all(|part| !part.is_empty() && part != "." && part != ".."),
                "invalid folder path in {repository}"
            );
            let prefixes = buckets.entry(bucket.to_owned()).or_default();
            for existing in prefixes.iter() {
                ensure!(
                    !contains(existing, prefix) && !contains(prefix, existing),
                    "duplicate or overlapping repositories: s3://{bucket}/{existing} and {repository}"
                );
            }
            prefixes.push(prefix.to_owned());
        }
        Ok(buckets)
    }
}

pub fn contains(prefix: &str, path: &str) -> bool {
    prefix.is_empty()
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(repositories: &str) -> Result<Config> {
        Config::parse(&format!(
            "s3_endpoint = 'https://example.com'\ns3_region = 'test'\nrepositories = [{repositories}]"
        ))
    }

    #[test]
    fn defaults_and_multiple_buckets() {
        let config = config("'s3://one/a', 's3://two/b'").unwrap();
        assert_eq!(config.max_age_days, 7);
        assert_eq!(config.buckets().unwrap().len(), 2);
    }

    #[test]
    fn rejects_overlaps_in_either_order_and_duplicates() {
        for repos in [
            "'s3://one/a', 's3://one/a/b'",
            "'s3://one/a/b', 's3://one/a'",
            "'s3://one/a', 's3://one/a/'",
            "'s3://one', 's3://one/a'",
        ] {
            assert!(config(repos).is_err(), "{repos}");
        }
        assert!(config("'s3://one/a', 's3://one/abc'").is_ok());
    }

    #[test]
    fn rejects_invalid_paths() {
        for repo in [
            "https://one/a",
            "s3:///a",
            "s3://one/a//b",
            "s3://one/a/../b",
        ] {
            assert!(config(&format!("'{repo}'")).is_err());
        }
    }
}
