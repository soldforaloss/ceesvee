//! Row filtering: evaluate a (possibly nested) filter spec against a document's
//! data rows and return the absolute indices of the rows that match.
//!
//! The spec is "compiled" once (regexes built, comparison values normalised)
//! and then evaluated per row. Numeric comparisons reuse [`crate::analyze`] so
//! a value matches a numeric filter exactly when it counts as numeric for sort
//! and summaries.

use std::cmp::Ordering;

use regex::{Regex, RegexBuilder};

use crate::analyze;
use crate::document::Document;
use crate::dto::{Conjunction, FilterCondition, FilterGroup, FilterNode, FilterOp};
use crate::error::{AppError, AppResult};
use crate::schema::{self, CellState, ColumnSchema, TypedValue};

enum Compiled {
    Group {
        conj: Conjunction,
        nodes: Vec<Compiled>,
    },
    Condition {
        column: usize,
        test: Test,
    },
}

enum NumOp {
    Gt,
    Gte,
    Lt,
    Lte,
}

/// A compiled per-cell test. String variants carry the already-normalised
/// comparison value plus whether matching is case-sensitive.
enum Test {
    Equals(String, bool),
    NotEquals(String, bool),
    Contains(String, bool),
    NotContains(String, bool),
    StartsWith(String, bool),
    EndsWith(String, bool),
    Num(NumOp, f64),
    /// F31: range comparison against a column with a DECLARED orderable type
    /// (integer/decimal/float/date/datetime) — the comparison value is parsed
    /// under the schema (locale, input formats) and cells compare in typed
    /// order. Cells that are null-ish or invalid under the schema never match,
    /// mirroring how non-numeric cells never satisfy `Num`.
    Typed(Box<(ColumnSchema, NumOp, TypedValue)>),
    IsEmpty,
    NotEmpty,
    Regex(Regex),
}

/// Lower-case a string unless the comparison is case-sensitive.
fn norm(s: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        s.to_string()
    } else {
        s.to_lowercase()
    }
}

/// Evaluate a filter spec over every data row, returning matching absolute
/// row indices in document order. Streams through [`Document::visit_rows`],
/// so it works for both editable and indexed backings.
pub fn matching_rows(doc: &Document, spec: &FilterGroup) -> AppResult<Vec<usize>> {
    let compiled = compile_group(spec, &|col| doc.column_schema_at(col).cloned())?;
    let mut out = Vec::new();
    doc.visit_rows(0..doc.n_rows(), &mut |i, row| {
        if eval(&compiled, row) {
            out.push(i);
        }
        Ok(true)
    })?;
    Ok(out)
}

fn compile_group(
    g: &FilterGroup,
    schema_of: &dyn Fn(usize) -> Option<ColumnSchema>,
) -> AppResult<Compiled> {
    let mut nodes = Vec::with_capacity(g.nodes.len());
    for node in &g.nodes {
        nodes.push(match node {
            FilterNode::Condition(c) => compile_condition(c, schema_of)?,
            FilterNode::Group(sub) => compile_group(sub, schema_of)?,
        });
    }
    Ok(Compiled::Group {
        conj: g.conjunction,
        nodes,
    })
}

/// Compile a range condition (`> >= < <=`): typed against the column's
/// declared schema when it has an orderable type, the f64 heuristic
/// otherwise.
fn compile_range(
    c: &FilterCondition,
    op: NumOp,
    schema_of: &dyn Fn(usize) -> Option<ColumnSchema>,
) -> AppResult<Test> {
    if let Some(schema) = schema_of(c.column) {
        if schema.logical_type.is_numeric() || schema.logical_type.is_temporal() {
            let trimmed = c.value.trim();
            if trimmed.is_empty() {
                return Err(AppError::invalid("the comparison needs a value"));
            }
            let target = schema::parse_typed(trimmed, &schema).map_err(|reason| {
                AppError::invalid(format!(
                    "'{}' is not valid for this column's declared type: {reason}",
                    c.value
                ))
            })?;
            return Ok(Test::Typed(Box::new((schema, op, target))));
        }
    }
    Ok(Test::Num(op, parse_num(&c.value)?))
}

fn compile_condition(
    c: &FilterCondition,
    schema_of: &dyn Fn(usize) -> Option<ColumnSchema>,
) -> AppResult<Compiled> {
    let cs = c.case_sensitive;
    let v = norm(&c.value, cs);
    let test = match c.op {
        FilterOp::Equals => Test::Equals(v, cs),
        FilterOp::NotEquals => Test::NotEquals(v, cs),
        FilterOp::Contains => Test::Contains(v, cs),
        FilterOp::NotContains => Test::NotContains(v, cs),
        FilterOp::StartsWith => Test::StartsWith(v, cs),
        FilterOp::EndsWith => Test::EndsWith(v, cs),
        FilterOp::IsEmpty => Test::IsEmpty,
        FilterOp::NotEmpty => Test::NotEmpty,
        FilterOp::Gt => compile_range(c, NumOp::Gt, schema_of)?,
        FilterOp::Gte => compile_range(c, NumOp::Gte, schema_of)?,
        FilterOp::Lt => compile_range(c, NumOp::Lt, schema_of)?,
        FilterOp::Lte => compile_range(c, NumOp::Lte, schema_of)?,
        FilterOp::Regex => {
            let re = RegexBuilder::new(&c.value)
                .case_insensitive(!cs)
                .build()
                .map_err(|e| AppError::invalid(format!("invalid regular expression: {e}")))?;
            Test::Regex(re)
        }
    };
    Ok(Compiled::Condition {
        column: c.column,
        test,
    })
}

fn parse_num(value: &str) -> AppResult<f64> {
    analyze::as_number(value).ok_or_else(|| AppError::invalid(format!("'{value}' is not a number")))
}

fn eval(node: &Compiled, row: &[String]) -> bool {
    match node {
        Compiled::Group { conj, nodes } => {
            // An empty group is neutral — it matches every row regardless of the
            // conjunction (matching the builder's "matches every row" hint).
            if nodes.is_empty() {
                return true;
            }
            match conj {
                Conjunction::And => nodes.iter().all(|n| eval(n, row)),
                Conjunction::Or => nodes.iter().any(|n| eval(n, row)),
            }
        }
        Compiled::Condition { column, test } => {
            let cell = row.get(*column).map(String::as_str).unwrap_or("");
            eval_test(test, cell)
        }
    }
}

fn eval_test(test: &Test, cell: &str) -> bool {
    match test {
        Test::IsEmpty => cell.trim().is_empty(),
        Test::NotEmpty => !cell.trim().is_empty(),
        Test::Regex(re) => re.is_match(cell),
        Test::Num(op, target) => match analyze::as_number(cell) {
            Some(n) => match op {
                NumOp::Gt => n > *target,
                NumOp::Gte => n >= *target,
                NumOp::Lt => n < *target,
                NumOp::Lte => n <= *target,
            },
            // A non-numeric cell never satisfies a numeric comparison.
            None => false,
        },
        Test::Typed(boxed) => {
            let (schema, op, target) = boxed.as_ref();
            match schema::classify(Some(cell), schema) {
                CellState::Valid(v) => {
                    let ord = schema::compare_typed(&v, target);
                    match op {
                        NumOp::Gt => ord == Ordering::Greater,
                        NumOp::Gte => ord != Ordering::Less,
                        NumOp::Lt => ord == Ordering::Less,
                        NumOp::Lte => ord != Ordering::Greater,
                    }
                }
                // Null-ish and invalid cells never satisfy a range test.
                _ => false,
            }
        }
        Test::Equals(v, cs) => &norm(cell, *cs) == v,
        Test::NotEquals(v, cs) => &norm(cell, *cs) != v,
        Test::Contains(v, cs) => norm(cell, *cs).contains(v.as_str()),
        Test::NotContains(v, cs) => !norm(cell, *cs).contains(v.as_str()),
        Test::StartsWith(v, cs) => norm(cell, *cs).starts_with(v.as_str()),
        Test::EndsWith(v, cs) => norm(cell, *cs).ends_with(v.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, ParseSettings};

    fn doc(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn cond(column: usize, op: FilterOp, value: &str) -> FilterNode {
        FilterNode::Condition(FilterCondition {
            column,
            op,
            value: value.to_string(),
            case_sensitive: false,
        })
    }

    fn group(conj: Conjunction, nodes: Vec<FilterNode>) -> FilterGroup {
        FilterGroup {
            conjunction: conj,
            nodes,
        }
    }

    #[test]
    fn contains_case_insensitive() {
        let d = doc("name,qty\nApple,3\nBanana,7\napricot,2");
        let g = group(Conjunction::And, vec![cond(0, FilterOp::Contains, "ap")]);
        // "Apple" and "apricot" contain "ap" (case-insensitive).
        assert_eq!(matching_rows(&d, &g).unwrap(), vec![0, 2]);
    }

    #[test]
    fn numeric_gt_ignores_non_numbers() {
        let d = doc("name,qty\na,3\nb,n/a\nc,10");
        let g = group(Conjunction::And, vec![cond(1, FilterOp::Gt, "5")]);
        assert_eq!(matching_rows(&d, &g).unwrap(), vec![2]);
    }

    #[test]
    fn and_or_nesting() {
        let d = doc("name,qty\na,3\nb,7\nc,12");
        // qty > 5 AND (name == a OR qty < 10):
        //   a(3): 3>5 -> no; b(7): 7>5 && (no || 7<10) -> yes; c(12): 12>5 && (no || no) -> no
        let inner = FilterNode::Group(group(
            Conjunction::Or,
            vec![cond(0, FilterOp::Equals, "a"), cond(1, FilterOp::Lt, "10")],
        ));
        let g = group(Conjunction::And, vec![cond(1, FilterOp::Gt, "5"), inner]);
        assert_eq!(matching_rows(&d, &g).unwrap(), vec![1]);
    }

    #[test]
    fn empty_group_matches_all() {
        let d = doc("name\na\nb");
        let g = group(Conjunction::And, vec![]);
        assert_eq!(matching_rows(&d, &g).unwrap(), vec![0, 1]);
    }

    #[test]
    fn empty_or_group_matches_all() {
        let d = doc("name\na\nb");
        let g = group(Conjunction::Or, vec![]);
        assert_eq!(matching_rows(&d, &g).unwrap(), vec![0, 1]);
    }

    #[test]
    fn bad_regex_errors() {
        let d = doc("name\na");
        let g = group(Conjunction::And, vec![cond(0, FilterOp::Regex, "(")]);
        assert!(matching_rows(&d, &g).is_err());
    }

    // ----- read/translate paths under an active filter ---------------------

    #[test]
    fn filtered_reads_map_display_to_absolute() {
        let mut d = doc("name,qty\na,1\nb,2\nc,3\nd,4");
        d.set_filter(vec![1, 3]).unwrap(); // keep absolute data rows b,2 and d,4
        assert_eq!(d.visible_len(), 2);
        assert_eq!(d.display_to_abs(0), Some(1));
        assert_eq!(d.display_to_abs(1), Some(3));
        assert_eq!(d.display_to_abs(2), None);
        assert_eq!(d.display_to_abs_insert(2), Some(4)); // append at end
        let resp = d.get_rows(0, 10).unwrap();
        assert_eq!(resp.rows.len(), 2);
        assert_eq!(resp.rows[0][0], "b");
        assert_eq!(resp.rows[1][0], "d");
    }

    #[test]
    fn filtered_selection_stats_use_visible_rows() {
        let mut d = doc("name,qty\na,1\nb,2\nc,3\nd,4");
        d.set_filter(vec![1, 3]).unwrap();
        let rect = crate::dto::CellRect {
            x: 1,
            y: 0,
            width: 1,
            height: 2,
        };
        let stats = d.selection_stats(rect).unwrap();
        assert_eq!(stats.numeric_count, 2);
        assert_eq!(stats.sum, 6.0); // 2 + 4, not the hidden 1 or 3
    }

    #[test]
    fn single_cell_fetch_respects_filter_and_full_content() {
        // The F13 cell editor reads one cell through display->abs + fetch:
        // the COMPLETE value (embedded newline included) must come back for
        // the row the filter actually shows.
        let mut d = doc("name,note\na,\"line1\nline2\"\nb,short");
        d.set_filter(vec![0]).unwrap();
        let abs = d.display_to_abs(0).unwrap();
        let rows = d.fetch_rows(&[abs]).unwrap();
        assert_eq!(rows[0][1], "line1\nline2");
    }

    // ----- typed range filters under a declared schema (F31) ----------------

    fn declare_with(
        d: &mut Document,
        col: usize,
        lt: crate::schema::LogicalType,
        f: impl FnOnce(&mut crate::schema::ColumnSchema),
    ) {
        let mut schema = crate::schema::ColumnSchema::new(
            d.column_ids()[col].clone(),
            d.headers()[col].clone(),
            lt,
        );
        f(&mut schema);
        d.set_column_schema(schema);
    }

    #[test]
    fn declared_locale_decimal_gt_compares_numerically() {
        // de-DE decimals: "1.234,5" is 1234.5. The f64 heuristic cannot read
        // them at all; the declared type must. (Values quoted — comma is the
        // CSV delimiter.)
        let mut d = doc("amount,who\n\"1.234,5\",a\n\"999,5\",b\nabc,c\n");
        declare_with(&mut d, 0, crate::schema::LogicalType::Decimal, |s| {
            s.locale = Some("de-DE".to_string());
        });
        let g = group(Conjunction::And, vec![cond(0, FilterOp::Gt, "1000")]);
        // Only 1234,5 exceeds 1000; "abc" (invalid) never matches.
        assert_eq!(matching_rows(&d, &g).unwrap(), vec![0]);
    }

    #[test]
    fn declared_date_range_compares_chronologically() {
        let mut d = doc("when\n31.12.2023\n02.01.2024\n");
        declare_with(&mut d, 0, crate::schema::LogicalType::Date, |s| {
            s.input_formats = Some(vec!["%d.%m.%Y".to_string()]);
        });
        // Lexicographically "31.12.2023" > "02.01.2024"; chronologically not.
        let g = group(Conjunction::And, vec![cond(0, FilterOp::Lt, "01.01.2024")]);
        assert_eq!(matching_rows(&d, &g).unwrap(), vec![0]);
        // A comparison value the declared type cannot parse errors up front.
        let bad = group(Conjunction::And, vec![cond(0, FilterOp::Lt, "not-a-date")]);
        assert!(matching_rows(&d, &bad).is_err());
    }

    #[test]
    fn null_token_and_empty_stay_distinguishable_in_filters() {
        // Second column keeps the empty-cell row from being an empty line
        // (which the parser would skip entirely).
        let mut d = doc("n,k\n1,a\nNULL,b\n,c\n2,d\n");
        declare_with(&mut d, 0, crate::schema::LogicalType::Integer, |s| {
            s.null_tokens = vec!["NULL".to_string()];
        });
        // IsEmpty matches ONLY the genuinely empty cell, never the token…
        let empty = group(Conjunction::And, vec![cond(0, FilterOp::IsEmpty, "")]);
        assert_eq!(matching_rows(&d, &empty).unwrap(), vec![2]);
        // …the token is still addressable as text…
        let token = group(Conjunction::And, vec![cond(0, FilterOp::Equals, "NULL")]);
        assert_eq!(matching_rows(&d, &token).unwrap(), vec![1]);
        // …and neither satisfies a typed range test.
        let range = group(Conjunction::And, vec![cond(0, FilterOp::Gte, "0")]);
        assert_eq!(matching_rows(&d, &range).unwrap(), vec![0, 3]);
    }

    #[test]
    fn declared_text_column_keeps_heuristic_numeric_filter() {
        // A ZIP-like column declared text: range filters fall back to the
        // f64 heuristic instead of erroring, matching pre-schema behavior.
        let mut d = doc("zip\n00501\n10001\n");
        declare_with(&mut d, 0, crate::schema::LogicalType::Text, |_| {});
        let g = group(Conjunction::And, vec![cond(0, FilterOp::Gt, "5000")]);
        assert_eq!(matching_rows(&d, &g).unwrap(), vec![1]);
    }

    #[test]
    fn find_under_filter_is_display_coords_and_panic_free() {
        let mut d = doc("name,qty\nx,1\ny,2\nx,3");
        d.set_filter(vec![2]).unwrap(); // only the third data row (x,3) visible -> display row 0
        let opts = crate::dto::FindOptions {
            query: "x".into(),
            ..Default::default()
        };
        let m = crate::find::find(&d, &opts).unwrap();
        assert_eq!(m, vec![crate::dto::FindMatch { row: 0, col: 0 }]);
        // A selection rect that exceeds the visible range must not panic.
        let oversized = crate::dto::FindOptions {
            query: "x".into(),
            selection: Some(crate::dto::CellRect {
                x: 0,
                y: 0,
                width: 9,
                height: 99,
            }),
            ..Default::default()
        };
        assert!(crate::find::find(&d, &oversized).is_ok());
    }
}
