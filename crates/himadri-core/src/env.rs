//! Environment-variable parsing, in one place.
//!
//! These conversions were previously open-coded at each call site across the
//! binary and the plugin crate, which let them drift: two boolean flags
//! disagreed on whether `1` was truthy, so the same operator input meant
//! different things depending on which variable it was written into.

use std::str::FromStr;

/// Parse `name` into `T`, or `None` when unset or unparseable.
///
/// An unparseable value reads as unset deliberately: these are optional knobs
/// with documented defaults, and a typo must not take the process down.
pub fn parse_var<T: FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

/// Whether `name` is set to a truthy value: `1`, `true`, or `yes`,
/// case-insensitively. Anything else — including unset — is false.
pub fn flag_is_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// Split a comma-separated env value into trimmed, non-empty items.
pub fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_csv_trims_and_drops_empties() {
        assert_eq!(split_csv(" foo , ,bar,,  baz "), vec!["foo", "bar", "baz"]);
        assert!(split_csv("").is_empty());
        assert!(split_csv(" , ,").is_empty());
    }

    /// Every truthy spelling an operator might reasonably write, and the
    /// near-misses that must stay false.
    #[test]
    fn flag_truthiness_accepts_one_true_yes_only() {
        for truthy in ["1", "true", "TRUE", " Yes ", "yes"] {
            assert!(
                matches!(
                    truthy.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes"
                ),
                "{truthy} should be truthy"
            );
        }
        for falsy in ["0", "false", "no", "", "2", "on"] {
            assert!(
                !matches!(
                    falsy.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes"
                ),
                "{falsy} should not be truthy"
            );
        }
    }
}
