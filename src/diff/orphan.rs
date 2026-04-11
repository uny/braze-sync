//! `[ARCHIVED-YYYY-MM-DD]` orphan rename helper. See IMPLEMENTATION.md §11.6.
//!
//! Content Block (Phase B1) and Email Template (future Phase B2) cannot
//! be deleted via the Braze API. When the apply path encounters an
//! orphan and the user has passed `--archive-orphans`, braze-sync
//! renames the remote resource by prefixing its name with
//! `[ARCHIVED-YYYY-MM-DD] ` so the operator can spot it in the Braze
//! dashboard. The data is never silently dropped.
//!
//! All functions in this module are pure and date-injectable so the
//! tests can lock the format without coupling to the system clock. The
//! CLI layer passes `chrono::Local::now().date_naive()` at call time.

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

/// Match `NNNN-NN-NN` without bothering to validate the actual calendar
/// date. The whole point of this check is to detect already-archived
/// names cheaply, not to validate the date itself — that distinction
/// matters because we never want a malformed prefix to silently strip
/// `[ARCHIVED-...]` and re-archive on top of it.
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
        // Defensive: an empty name shouldn't crash. The validate layer
        // is supposed to reject these, but the helper should be total.
        let renamed = archive_name(day(2026, 4, 11), "");
        assert_eq!(renamed, "[ARCHIVED-2026-04-11] ");
    }
}
