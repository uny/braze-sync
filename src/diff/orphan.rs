//! `[ARCHIVED-YYYY-MM-DD]` orphan rename helper.
//!
//! Resources with no DELETE endpoint get renamed instead of dropped, so
//! operators can still find them in the Braze dashboard. Functions are
//! pure and date-injectable so tests can lock the format without
//! coupling to the system clock.

use chrono::NaiveDate;

const PREFIX_OPEN: &str = "[ARCHIVED-";
const PREFIX_CLOSE: &str = "] ";

/// Apply the archive prefix to `original`. Idempotent: if the name
/// already begins with `[ARCHIVED-YYYY-MM-DD] `, return it unchanged so
/// running `apply --archive-orphans` twice does not produce
/// `[ARCHIVED-2026-04-11] [ARCHIVED-2026-04-10] foo`.
pub fn archive_name(today: NaiveDate, original: &str) -> String {
    if is_archived(original) {
        return original.to_string();
    }
    format!(
        "{PREFIX_OPEN}{}{PREFIX_CLOSE}{original}",
        today.format("%Y-%m-%d")
    )
}

/// Whether `name` already carries an archive prefix in the canonical
/// `[ARCHIVED-YYYY-MM-DD] ` shape. The date itself is parsed loosely
/// (any 4-2-2 digit triple) so a name from a different day is still
/// recognized as already-archived.
pub fn is_archived(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(PREFIX_OPEN) else {
        return false;
    };
    let Some(close_idx) = rest.find(PREFIX_CLOSE) else {
        return false;
    };
    let date_part = &rest[..close_idx];
    looks_like_date(date_part)
}

/// Match `NNNN-NN-NN` without validating the actual calendar date —
/// permissive on purpose so a wrong-day prefix is still recognized as
/// already-archived (otherwise we'd re-stamp on top of it).
fn looks_like_date(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 10 {
        return false;
    }
    bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && bytes[8..].iter().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn archive_prefixes_a_clean_name() {
        let renamed = archive_name(day(2026, 4, 11), "promo");
        assert_eq!(renamed, "[ARCHIVED-2026-04-11] promo");
    }

    #[test]
    fn archive_pads_single_digit_month_and_day() {
        let renamed = archive_name(day(2026, 1, 9), "x");
        assert_eq!(renamed, "[ARCHIVED-2026-01-09] x");
    }

    #[test]
    fn archive_is_idempotent_on_already_archived_name() {
        let once = archive_name(day(2026, 4, 11), "promo");
        let twice = archive_name(day(2026, 4, 12), &once);
        // Same date is preserved — we don't re-stamp the prefix.
        assert_eq!(twice, once);
    }

    #[test]
    fn is_archived_recognizes_canonical_prefix() {
        assert!(is_archived("[ARCHIVED-2026-04-11] x"));
        assert!(is_archived("[ARCHIVED-1999-12-31] legacy thing"));
    }

    #[test]
    fn is_archived_rejects_close_but_wrong_shapes() {
        assert!(!is_archived("ARCHIVED-2026-04-11 x"));
        assert!(!is_archived("[ARCHIVED-2026/04/11] x"));
        assert!(!is_archived("[ARCHIVED-26-4-11] x"));
        assert!(!is_archived("[ARCHIVED-] x"));
        assert!(!is_archived("[ARCHIVED-2026-04-11]x")); // missing space
        assert!(!is_archived("plain name"));
    }

    #[test]
    fn empty_original_is_still_archived() {
        let renamed = archive_name(day(2026, 4, 11), "");
        assert_eq!(renamed, "[ARCHIVED-2026-04-11] ");
    }
}
