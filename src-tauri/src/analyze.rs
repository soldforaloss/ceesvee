//! Shared cell-value coercion and per-column type detection.
//!
//! Every "is this a number / date / bool?" decision in CEESVEE funnels through
//! this module so sorting, selection statistics, column summaries and filtering
//! all agree on what each type means. The numeric rule mirrors [`crate::sort`]
//! exactly: only a genuinely *finite* f64 counts (literal "nan"/"inf" parse as
//! f64 but are treated as text), which preserves a strict-weak ordering.

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime};

/// Parse a (possibly untrimmed) cell as a finite number. The single source of
/// truth for "is numeric" across the app.
pub fn as_number(cell: &str) -> Option<f64> {
    cell.trim().parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Whether a cell reads as a boolean. Excludes "0"/"1" on purpose so numeric
/// flag columns stay numeric instead of being mistaken for booleans.
pub fn is_bool(cell: &str) -> bool {
    matches!(
        cell.trim().to_ascii_lowercase().as_str(),
        "true" | "false" | "yes" | "no" | "y" | "n" | "t" | "f"
    )
}

/// Date formats recognised for type detection. Both US (M/D/Y) and
/// international (D/M/Y) slash orders are accepted; for a type badge the exact
/// order is immaterial, and chrono rejects impossible dates either way.
const DATE_FORMATS: &[&str] = &[
    "%Y-%m-%d", "%Y/%m/%d", "%m/%d/%Y", "%d/%m/%Y", "%m-%d-%Y", "%d-%m-%Y", "%d.%m.%Y", "%Y.%m.%d",
];

/// Date-time formats recognised in addition to plain dates.
const DATETIME_FORMATS: &[&str] = &[
    "%Y-%m-%d %H:%M:%S",
    "%Y-%m-%d %H:%M",
    "%Y/%m/%d %H:%M:%S",
    "%m/%d/%Y %H:%M:%S",
    "%Y-%m-%dT%H:%M:%S",
];

/// Whether a cell parses as a real calendar date or date-time. Uses chrono so
/// impossible values like "2024-13-40" are rejected.
pub fn is_date(cell: &str) -> bool {
    let s = cell.trim();
    if s.is_empty() {
        return false;
    }
    // Require a 4-digit year (>= 1000): chrono's `%Y` otherwise accepts a 1-3
    // digit year, which would mis-classify short hierarchical codes such as
    // "1.2.3" or "1/2/3" (parsed as year 3) as dates.
    let year_ok = |y: i32| (1000..=9999).contains(&y);
    for fmt in DATE_FORMATS {
        if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
            if year_ok(d.year()) {
                return true;
            }
        }
    }
    for fmt in DATETIME_FORMATS {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            if year_ok(dt.year()) {
                return true;
            }
        }
    }
    DateTime::parse_from_rfc3339(s).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_finite_only() {
        assert_eq!(as_number(" 3.5 "), Some(3.5));
        assert_eq!(as_number("-2"), Some(-2.0));
        assert!(as_number("nan").is_none());
        assert!(as_number("inf").is_none());
        assert!(as_number("abc").is_none());
    }

    #[test]
    fn bools_exclude_numeric_flags() {
        assert!(is_bool("true"));
        assert!(is_bool("No"));
        assert!(is_bool("Y"));
        assert!(!is_bool("0"));
        assert!(!is_bool("1"));
        assert!(!is_bool("maybe"));
    }

    #[test]
    fn dates_validate_calendar() {
        assert!(is_date("2024-01-31"));
        assert!(is_date("01/31/2024"));
        assert!(is_date("2024-01-02T03:04:05Z"));
        assert!(!is_date("2024-13-40"));
        assert!(!is_date("hello"));
        // Short version-like codes must NOT be treated as dates.
        assert!(!is_date("1.2.3"));
        assert!(!is_date("1/2/3"));
        assert!(!is_date("1-2-3"));
    }
}
