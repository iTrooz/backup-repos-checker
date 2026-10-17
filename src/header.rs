use anyhow::{Context, Result, ensure};
use globset::{GlobBuilder, GlobMatcher};
use regex::bytes::{Regex, RegexBuilder};
use serde::Deserialize;

use crate::storage::Storage;

pub const HEADER_BYTES: usize = 64;
pub const CHECK_CONCURRENCY: usize = 8;
pub const MAX_FAILURE_SAMPLES: usize = 10;

#[derive(Debug, Deserialize)]
#[serde(try_from = "HeaderConfig")]
pub struct HeaderCheck {
    rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeaderConfig {
    rules: Vec<RuleConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleConfig {
    glob: String,
    pattern: String,
}

#[derive(Debug)]
struct Rule {
    glob: GlobMatcher,
    pattern: Regex,
}

impl TryFrom<HeaderConfig> for HeaderCheck {
    type Error = anyhow::Error;

    fn try_from(config: HeaderConfig) -> Result<Self> {
        ensure!(
            !config.rules.is_empty(),
            "header_check requires at least one rule"
        );
        let rules = config
            .rules
            .into_iter()
            .map(|rule| {
                let glob = GlobBuilder::new(&rule.glob)
                    .literal_separator(true)
                    .build()
                    .with_context(|| format!("invalid header-check glob {:?}", rule.glob))?
                    .compile_matcher();
                let pattern = RegexBuilder::new(&rule.pattern)
                    .unicode(false)
                    .dot_matches_new_line(true)
                    .build()
                    .with_context(|| format!("invalid byte pattern {:?}", rule.pattern))?;
                Ok(Rule { glob, pattern })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { rules })
    }
}

impl HeaderCheck {
    /// The first matching rule wins. Every non-marker file must match a rule.
    /// A returned string is a validation failure; storage errors remain errors.
    pub async fn check(
        &self,
        storage: &dyn Storage,
        key: &str,
        relative: &str,
    ) -> Result<Option<String>> {
        let Some(rule) = self.rules.iter().find(|rule| rule.glob.is_match(relative)) else {
            return Ok(Some("no matching header rule".to_owned()));
        };
        let bytes = storage
            .read_prefix(key, HEADER_BYTES)
            .await
            .with_context(|| format!("cannot read header of {key:?}"))?;
        Ok((!rule
            .pattern
            .is_match(&bytes[..bytes.len().min(HEADER_BYTES)]))
        .then(|| "header does not match pattern".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::FileEntry;
    use async_trait::async_trait;
    use futures::stream::BoxStream;

    struct Bytes(Vec<u8>);

    #[async_trait]
    impl Storage for Bytes {
        async fn list(&self) -> Result<BoxStream<'_, Result<FileEntry>>> {
            unreachable!()
        }

        async fn read_prefix(&self, _path: &str, length: usize) -> Result<Vec<u8>> {
            assert_eq!(length, HEADER_BYTES);
            Ok(self.0.iter().copied().take(length).collect())
        }
    }

    fn rules(glob: &str, pattern: &str) -> HeaderCheck {
        HeaderConfig {
            rules: vec![RuleConfig {
                glob: glob.into(),
                pattern: pattern.into(),
            }],
        }
        .try_into()
        .unwrap()
    }

    #[tokio::test]
    async fn binary_patterns_support_alternatives_and_newline_wildcards() {
        let check = rules("**/*.gz", r"^(\x1f\x8b\x08|MAGIC).{2}\xff");
        let bytes = Bytes(vec![0x1f, 0x8b, 0x08, b'\n', 0, 0xff]);
        assert!(
            check
                .check(&bytes, "repo/nested/file.gz", "nested/file.gz")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            check
                .check(&Bytes(b"MAGIC\n\0\xff".to_vec()), "file.gz", "file.gz")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn rejects_short_empty_and_unmatched_files() {
        let check = rules("**/*.gz", r"^\x1f\x8b\x08");
        for bytes in [vec![], vec![0x1f], b"bad".to_vec()] {
            assert!(
                check
                    .check(&Bytes(bytes), "file.gz", "file.gz")
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        assert_eq!(
            check
                .check(&Bytes(vec![]), "file.zip", "file.zip")
                .await
                .unwrap()
                .as_deref(),
            Some("no matching header rule")
        );
    }

    #[test]
    fn invalid_rules_are_rejected() {
        for (glob, pattern) in [("[", "^x"), ("**", "(")] {
            assert!(
                HeaderCheck::try_from(HeaderConfig {
                    rules: vec![RuleConfig {
                        glob: glob.into(),
                        pattern: pattern.into()
                    }],
                })
                .is_err()
            );
        }
        assert!(HeaderCheck::try_from(HeaderConfig { rules: vec![] }).is_err());
    }

    #[tokio::test]
    async fn first_matching_rule_wins_and_only_prefix_is_examined() {
        let check = HeaderCheck::try_from(HeaderConfig {
            rules: vec![
                RuleConfig {
                    glob: "special".into(),
                    pattern: "^SPECIAL".into(),
                },
                RuleConfig {
                    glob: "**".into(),
                    pattern: "^OTHER".into(),
                },
            ],
        })
        .unwrap();
        assert!(
            check
                .check(&Bytes(b"OTHER".to_vec()), "special", "special")
                .await
                .unwrap()
                .is_some()
        );
        let check = rules("**", "MAGIC");
        let mut bytes = vec![0; HEADER_BYTES];
        bytes.extend_from_slice(b"MAGIC");
        assert!(
            check
                .check(&Bytes(bytes), "file", "file")
                .await
                .unwrap()
                .is_some()
        );
    }
}
