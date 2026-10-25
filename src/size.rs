//! Parse configured byte limits and format measured sizes for display.

use anyhow::{Result, bail, ensure};
use serde::Deserialize;

/// Binary units accepted in configuration, paired with their display spelling.
const UNITS: [(&str, u64); 6] = [
    ("B", 1),
    ("KiB", 1 << 10),
    ("MiB", 1 << 20),
    ("GiB", 1 << 30),
    ("TiB", 1 << 40),
    ("PiB", 1 << 50),
];

/// Configured byte limit, parsed while configuration is loaded.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(try_from = "String")]
pub struct Limit(pub u64);

impl TryFrom<String> for Limit {
    type Error = anyhow::Error;

    /// Reject malformed or zero limits as soon as the configuration is deserialized.
    fn try_from(text: String) -> Result<Self> {
        parse(&text).map(Self)
    }
}

/// Parse an integer byte count with an optional binary unit, such as "100GiB".
pub fn parse(text: &str) -> Result<u64> {
    let text = text.trim();
    ensure!(!text.is_empty(), "size cannot be empty");

    let split = text
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    ensure!(
        !number.is_empty(),
        "size must start with a whole number of bytes: {text:?}"
    );

    let value: u64 = number
        .parse()
        .map_err(|_| anyhow::anyhow!("size is too large: {text:?}"))?;
    let unit = unit.trim();
    let multiplier = if unit.is_empty() {
        1
    } else {
        UNITS
            .iter()
            .find(|(name, _)| {
                name.eq_ignore_ascii_case(unit)
                    || (unit.len() == 1 && name[..1].eq_ignore_ascii_case(unit))
            })
            .map(|(_, multiplier)| *multiplier)
            .ok_or_else(|| anyhow::anyhow!("unknown size unit in {text:?}"))?
    };

    let bytes = value
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow::anyhow!("size is too large: {text:?}"))?;
    if bytes == 0 {
        bail!("size must be positive");
    }

    Ok(bytes)
}

/// Format a byte count using the largest binary unit that keeps a readable value.
pub fn format(bytes: u64) -> String {
    let mut unit = 0;
    let mut value = bytes as f64;

    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }

    let value = if unit == 0 {
        bytes.to_string()
    } else {
        format!("{value:.1}").trim_end_matches(".0").to_owned()
    };

    format!("{value} {}", UNITS[unit].0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_human_readable_sizes() {
        assert_eq!(parse("1024").unwrap(), 1024);
        assert_eq!(parse(" 2 MiB ").unwrap(), 2 << 20);
        assert_eq!(parse("1gib").unwrap(), 1 << 30);
        assert_eq!(parse("1G").unwrap(), 1 << 30);
        assert_eq!(parse("3 TIB").unwrap(), 3 << 40);
        assert_eq!(parse("2m").unwrap(), 2 << 20);
    }

    #[test]
    fn rejects_malformed_and_zero_sizes() {
        for text in ["", "   ", "GiB", "1.5GiB", "0", "0 B", "12 XiB", "-1"] {
            assert!(parse(text).is_err(), "{text:?}");
        }
        assert!(parse("18446744073709551616").is_err());
        assert!(parse("9999999999 PiB").is_err());
    }

    #[test]
    fn formats_bytes_with_binary_units() {
        assert_eq!(format(0), "0 B");
        assert_eq!(format(512), "512 B");
        assert_eq!(format(1024), "1 KiB");
        assert_eq!(format(1536), "1.5 KiB");
        assert_eq!(format(100 << 30), "100 GiB");
    }
}
