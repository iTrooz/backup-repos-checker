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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    pub uri: String,
    pub header_check: Option<crate::header::HeaderCheck>,
}

#[derive(Clone, Copy)]
pub struct RepositoryLocation<'a> {
    pub prefix: &'a str,
    pub header_check: Option<&'a crate::header::HeaderCheck>,
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
                header_check: config.header_check.as_ref(),
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

    #[test]
    fn header_rules_are_validated_when_loading_config() {
        let base = "s3_endpoint = 'https://example.com'\ns3_region = 'test'\n[[repositories]]\nuri = 's3://bucket/repo'\n";
        for header in [
            "[repositories.header_check]\nrules = []",
            "[[repositories.header_check.rules]]\nglob = '**'\npattern = '('",
            "[[repositories.header_check.rules]]\nglob = '['\npattern = '^x'",
            "[repositories.header_check]\nread_bytes = 64\n[[repositories.header_check.rules]]\nglob = '**'\npattern = '^x'",
        ] {
            assert!(
                Config::parse(&format!("{base}{header}")).is_err(),
                "{header}"
            );
        }
        assert!(
            Config::parse(&format!(
                "{base}[[repositories.header_check.rules]]\nglob = '**'\npattern = '^\\xff'"
            ))
            .is_ok()
        );
    }
}
