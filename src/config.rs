use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub s3_endpoint: String,
    pub s3_region: String,
    #[serde(default)]
    pub checks: ChecksConfig,
    pub repositories: Vec<RepositoryConfig>,
    pub email: Option<crate::report::EmailConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChecksConfig {
    pub age: AgeCheckConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgeCheckConfig {
    pub max_age_days: u32,
}

impl Default for AgeCheckConfig {
    fn default() -> Self {
        Self { max_age_days: 7 }
    }
}

/// Repository location, content rules, and checks explicitly disabled for this repository.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    pub uri: String,
    pub content_check: Option<crate::findings::content::ContentCheck>,
    #[serde(default)]
    pub silence_checks: Vec<crate::findings::selection::Kind>,
}

/// Borrowed repository settings for scanning one bucket.
#[derive(Clone, Copy)]
pub struct RepositoryLocation<'a> {
    pub prefix: &'a str,
    pub content_check: Option<&'a crate::findings::content::ContentCheck>,
    pub silence_checks: &'a [crate::findings::selection::Kind],
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
        ensure!(
            config.checks.age.max_age_days > 0,
            "checks.age.max_age_days must be positive"
        );
        ensure!(
            !config.repositories.is_empty(),
            "configure at least one repository"
        );
        config.buckets()?;
        Ok(config)
    }

    /// Bucket names mapped to normalized repository prefixes (empty means bucket root).
    pub fn buckets(&self) -> Result<BTreeMap<String, Vec<RepositoryLocation<'_>>>> {
        let mut buckets: BTreeMap<String, Vec<RepositoryLocation<'_>>> = BTreeMap::new();
        for config in &self.repositories {
            let repository = &config.uri;
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
                let existing = existing.prefix;
                ensure!(
                    !contains(existing, prefix) && !contains(prefix, existing),
                    "duplicate or overlapping repositories: s3://{bucket}/{existing} and {repository}"
                );
            }
            prefixes.push(RepositoryLocation {
                prefix,
                content_check: config.content_check.as_ref(),
                silence_checks: &config.silence_checks,
            });
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
        let repositories = repositories
            .split(',')
            .map(|uri| format!("[[repositories]]\nuri = {}", uri.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        Config::parse(&format!(
            "s3_endpoint = 'https://example.com'\ns3_region = 'test'\n{repositories}"
        ))
    }

    #[test]
    fn defaults_and_multiple_buckets() {
        let config = config("'s3://one/a', 's3://two/b'").unwrap();
        assert_eq!(config.checks.age.max_age_days, 7);
        assert_eq!(config.buckets().unwrap().len(), 2);
    }

    #[test]
    fn age_check_section_defaults_and_overrides() {
        let base = "s3_endpoint = 'https://example.com'\ns3_region = 'test'\n[[repositories]]\nuri = 's3://bucket/repo'\n";
        for section in ["[checks]", "[checks.age]"] {
            let config = Config::parse(&format!("{base}{section}")).unwrap();
            assert_eq!(config.checks.age.max_age_days, 7);
        }
        let config = Config::parse(&format!("{base}[checks.age]\nmax_age_days = 14")).unwrap();
        assert_eq!(config.checks.age.max_age_days, 14);
        let error = Config::parse(&format!("{base}[checks.age]\nmax_age_days = 0")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("checks.age.max_age_days must be positive")
        );
        assert!(Config::parse(&format!("max_age_days = 7\n{base}")).is_err());
        assert!(Config::parse(&format!("{base}[checks.age]\nmax_age_days = -1")).is_err());
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

    #[test]
    fn example_configuration_is_valid() {
        Config::parse(include_str!("../config.example.toml")).unwrap();
    }

    /// Compile content rules during parsing and reject malformed configuration.
    #[test]
    fn content_rules_are_validated_when_loading_config() {
        let base = "s3_endpoint = 'https://example.com'\ns3_region = 'test'\n[[repositories]]\nuri = 's3://bucket/repo'\n";
        for content in [
            "[repositories.content_check]\nrules = []",
            "[[repositories.content_check.rules]]\nglob = '**'\npattern = '('",
            "[[repositories.content_check.rules]]\nglob = '['\npattern = '^x'",
            "[repositories.content_check]\nread_bytes = 64\n[[repositories.content_check.rules]]\nglob = '**'\npattern = '^x'",
        ] {
            assert!(
                Config::parse(&format!("{base}{content}")).is_err(),
                "{content}"
            );
        }
        assert!(
            Config::parse(&format!(
                "{base}[[repositories.content_check.rules]]\nglob = '**'\npattern = '^\\xff'"
            ))
            .is_ok()
        );
    }

    /// Silencing defaults to empty and accepts only known check names.
    #[test]
    fn repository_silencing_is_validated_and_forwarded() {
        let base = "s3_endpoint = 'https://example.com'\ns3_region = 'test'\n[[repositories]]\nuri = 's3://bucket/repo'\n";
        assert!(
            Config::parse(base).unwrap().repositories[0]
                .silence_checks
                .is_empty()
        );
        let config = Config::parse(&format!(
            "{base}silence_checks = ['folders', 'age', 'content']"
        ))
        .unwrap();
        let buckets = config.buckets().unwrap();
        let active = crate::findings::selection::Selection::default()
            .without(buckets["bucket"][0].silence_checks);
        assert!(!active.folders && !active.age && !active.content);
        for invalid in ["['unknown']", "['Age']", "'age'", "[1]"] {
            assert!(Config::parse(&format!("{base}silence_checks = {invalid}")).is_err());
        }
    }
}
