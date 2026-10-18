//! Validate file prefixes against repository-specific byte patterns.

use anyhow::{Context, Result, ensure};
use globset::{GlobBuilder, GlobMatcher};
use regex::bytes::{Regex, RegexBuilder};
use serde::Deserialize;

use crate::storage::Storage;

/// Maximum bytes examined per file; deliberately not configurable yet.
pub const PREFIX_BYTES: usize = 64;

/// Compiled repository-specific content validation rules.
#[derive(Debug, Deserialize)]
#[serde(try_from = "ContentConfig")]
pub struct ContentCheck {
    rules: Vec<Rule>,
}

/// Serializable rules before glob and byte-pattern compilation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentConfig {
    rules: Vec<RuleConfig>,
}

/// One path matcher and its expected byte pattern.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleConfig {
    glob: String,
    pattern: String,
}

/// Executable matchers compiled once during configuration loading.
#[derive(Debug)]
struct Rule {
    glob: GlobMatcher,
    pattern: Regex,
}

impl TryFrom<ContentConfig> for ContentCheck {
    type Error = anyhow::Error;

    /// Compile rules once when configuration is loaded.
    fn try_from(config: ContentConfig) -> Result<Self> {
        ensure!(
            !config.rules.is_empty(),
            "content_check requires at least one rule"
        );
        let rules = config
            .rules
            .into_iter()
            .map(|rule| {
                let glob = GlobBuilder::new(&rule.glob)
                    .literal_separator(true)
                    .build()
                    .with_context(|| format!("invalid content-check glob {:?}", rule.glob))?
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

impl ContentCheck {
    /// The first matching rule wins. Every non-marker file must match a rule.
    /// A returned string is a validation failure; storage errors remain errors.
    pub async fn check(
        &self,
        storage: &dyn Storage,
        key: &str,
        relative: &str,
    ) -> Result<Option<String>> {
        let Some(rule) = self.rules.iter().find(|rule| rule.glob.is_match(relative)) else {
            return Ok(Some("no matching content rule".to_owned()));
        };
        let bytes = storage
            .read_prefix(key, PREFIX_BYTES)
            .await
            .with_context(|| format!("cannot read content prefix of {key:?}"))?;
        Ok((!rule
            .pattern
            .is_match(&bytes[..bytes.len().min(PREFIX_BYTES)]))
        .then(|| "content does not match pattern".to_owned()))
    }
}

/// Byte-pattern semantics and configuration validation.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::FileEntry;
    use async_trait::async_trait;
    use futures::stream::BoxStream;

    /// A file-prefix fixture that does not require an object listing.
    struct Bytes(Vec<u8>);

    #[async_trait]
    impl Storage for Bytes {
        /// Pattern-only tests do not discover files.
        async fn list(&self) -> Result<BoxStream<'_, Result<FileEntry>>> {
            unreachable!()
        }

        /// Return a bounded prefix, preserving arbitrary binary bytes.
        async fn read_prefix(&self, _path: &str, length: usize) -> Result<Vec<u8>> {
            assert_eq!(length, PREFIX_BYTES);
            Ok(self.0.iter().copied().take(length).collect())
        }
    }

    /// Compile a single test rule using the normal configuration path.
    fn rules(glob: &str, pattern: &str) -> ContentCheck {
        ContentConfig {
            rules: vec![RuleConfig {
                glob: glob.into(),
                pattern: pattern.into(),
            }],
        }
        .try_into()
        .unwrap()
    }

    /// Patterns support binary alternatives and wildcard bytes including newlines.
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

    /// Incomplete prefixes and files without matching rules fail validation.
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
            Some("no matching content rule")
        );
    }

    /// Reject malformed rules and an explicitly empty rule list.
    #[test]
    fn invalid_rules_are_rejected() {
        for (glob, pattern) in [("[", "^x"), ("**", "(")] {
            assert!(
                ContentCheck::try_from(ContentConfig {
                    rules: vec![RuleConfig {
                        glob: glob.into(),
                        pattern: pattern.into()
                    }],
                })
                .is_err()
            );
        }
        assert!(ContentCheck::try_from(ContentConfig { rules: vec![] }).is_err());
    }

    /// Rule order determines precedence and bytes after the prefix are ignored.
    #[tokio::test]
    async fn first_matching_rule_wins_and_only_prefix_is_examined() {
        let check = ContentCheck::try_from(ContentConfig {
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
        let mut bytes = vec![0; PREFIX_BYTES];
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
