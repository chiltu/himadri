//! Timestamp parsing shared by the SQLite-backed stores.
//!
//! Rows are written with an explicit RFC3339 timestamp bound from Rust
//! (`Utc::now().to_rfc3339()`) rather than SQLite's `datetime('now')`, whose
//! `YYYY-MM-DD HH:MM:SS` output isn't RFC3339 (no `T`, no offset) and both
//! fails to parse as RFC3339 and — used directly in a `WHERE ... > ?`
//! comparison — sorts incorrectly against RFC3339 strings. `parse` still
//! accepts that legacy format so rows written before this fix keep working.

use chrono::{DateTime, NaiveDateTime, Utc};

pub fn parse(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|naive| naive.and_utc())
}

pub fn parse_or_default(s: &str) -> DateTime<Utc> {
    parse(s).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc3339() {
        let dt = parse("2026-07-01T12:33:03+00:00").unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-07-01T12:33:03+00:00");
    }

    #[test]
    fn parses_legacy_sqlite_format() {
        let dt = parse("2026-07-01 12:33:03").unwrap();
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-07-01 12:33:03"
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("not-a-date").is_none());
    }
}
