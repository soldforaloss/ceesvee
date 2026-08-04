//! Multi-key row comparison used by [`crate::document::Document::sort`].

use std::cmp::Ordering;

use crate::dto::SortKey;
use crate::schema::ColumnSchema;

/// Compare two rows by an ordered list of sort keys, where a key whose
/// column has a DECLARED schema (F31)
/// compares in typed order instead of the text/f64 heuristics: integers as
/// i128, decimals exactly, dates chronologically, text lexicographically.
/// Null-ish cells (empty or a configured null token) sort first ascending and
/// invalid cells last — see [`crate::schema::compare_cells`]. `schemas` runs
/// parallel to `keys` (missing/`None` entries keep the heuristic order).
pub fn compare_rows_schema(
    a: &[String],
    b: &[String],
    keys: &[SortKey],
    schemas: &[Option<ColumnSchema>],
) -> Ordering {
    for (i, key) in keys.iter().enumerate() {
        let av = a.get(key.column).map(String::as_str).unwrap_or("");
        let bv = b.get(key.column).map(String::as_str).unwrap_or("");
        let mut ord = match schemas.get(i).and_then(Option::as_ref) {
            Some(schema) => crate::schema::compare_cells(schema, av, bv),
            None => compare_values(av, bv),
        };
        if key.descending {
            ord = ord.reverse();
        }
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

fn compare_values(a: &str, b: &str) -> Ordering {
    match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
        // Only treat genuinely finite numbers as numeric. Literal text like
        // "nan"/"inf" parses as f64 but would break the strict-weak-ordering
        // contract (NaN compares Equal to everything), so fall back to text.
        (Ok(x), Ok(y)) if x.is_finite() && y.is_finite() => {
            x.partial_cmp(&y).unwrap_or(Ordering::Equal)
        }
        _ => a.cmp(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// The schema-less (heuristic) order the pre-F31 tests exercise.
    fn compare_rows(a: &[String], b: &[String], keys: &[SortKey]) -> Ordering {
        compare_rows_schema(a, b, keys, &[])
    }

    #[test]
    fn numeric_when_both_numeric() {
        let keys = [SortKey {
            column: 0,
            descending: false,
        }];
        // "9" vs "10": numeric => 9 < 10 (lexicographic would say "10" < "9").
        assert_eq!(
            compare_rows(&row(&["9"]), &row(&["10"]), &keys),
            Ordering::Less
        );
    }

    #[test]
    fn lexicographic_when_text() {
        let keys = [SortKey {
            column: 0,
            descending: false,
        }];
        assert_eq!(
            compare_rows(&row(&["apple"]), &row(&["banana"]), &keys),
            Ordering::Less
        );
    }

    #[test]
    fn descending_reverses() {
        let keys = [SortKey {
            column: 0,
            descending: true,
        }];
        assert_eq!(
            compare_rows(&row(&["1"]), &row(&["2"]), &keys),
            Ordering::Greater
        );
    }

    #[test]
    fn nan_and_inf_text_sorts_lexicographically() {
        let keys = [SortKey {
            column: 0,
            descending: false,
        }];
        // "nan"/"inf" parse as f64 but must NOT take the numeric path (NaN would
        // break ordering). They compare as text instead.
        assert_eq!(
            compare_rows(&row(&["inf"]), &row(&["apple"]), &keys),
            Ordering::Greater // 'i' > 'a' lexicographically
        );
        // A literal "nan" must compare consistently (text), not pin as Equal.
        assert_ne!(
            compare_rows(&row(&["nan"]), &row(&["1"]), &keys),
            Ordering::Equal
        );
    }

    #[test]
    fn secondary_key_breaks_ties() {
        let keys = [
            SortKey {
                column: 0,
                descending: false,
            },
            SortKey {
                column: 1,
                descending: false,
            },
        ];
        assert_eq!(
            compare_rows(&row(&["a", "2"]), &row(&["a", "1"]), &keys),
            Ordering::Greater
        );
    }
}
