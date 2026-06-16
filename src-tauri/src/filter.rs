//! Row filtering: evaluate a (possibly nested) filter spec against a document's
//! data rows and return the absolute indices of the rows that match.
//!
//! The spec is "compiled" once (regexes built, comparison values normalised)
//! and then evaluated per row. Numeric comparisons reuse [`crate::analyze`] so
//! a value matches a numeric filter exactly when it counts as numeric for sort
//! and summaries.

use regex::{Regex, RegexBuilder};

use crate::analyze;
use crate::document::Document;
use crate::dto::{Conjunction, FilterCondition, FilterGroup, FilterNode, FilterOp};
use crate::error::{AppError, AppResult};

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
/// row indices in document order.
pub fn matching_rows(doc: &Document, spec: &FilterGroup) -> AppResult<Vec<usize>> {
    let compiled = compile_group(spec)?;
    let mut out = Vec::new();
    for (i, row) in doc.rows().iter().enumerate() {
        if eval(&compiled, row) {
            out.push(i);
        }
    }
    Ok(out)
}

fn compile_group(g: &FilterGroup) -> AppResult<Compiled> {
    let mut nodes = Vec::with_capacity(g.nodes.len());
    for node in &g.nodes {
        nodes.push(match node {
            FilterNode::Condition(c) => compile_condition(c)?,
            FilterNode::Group(sub) => compile_group(sub)?,
        });
    }
    Ok(Compiled::Group {
        conj: g.conjunction,
        nodes,
    })
}

fn compile_condition(c: &FilterCondition) -> AppResult<Compiled> {
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
        FilterOp::Gt => Test::Num(NumOp::Gt, parse_num(&c.value)?),
        FilterOp::Gte => Test::Num(NumOp::Gte, parse_num(&c.value)?),
        FilterOp::Lt => Test::Num(NumOp::Lt, parse_num(&c.value)?),
        FilterOp::Lte => Test::Num(NumOp::Lte, parse_num(&c.value)?),
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
        Compiled::Group { conj, nodes } => match conj {
            Conjunction::And => nodes.iter().all(|n| eval(n, row)),
            Conjunction::Or => !nodes.is_empty() && nodes.iter().any(|n| eval(n, row)),
        },
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
    fn bad_regex_errors() {
        let d = doc("name\na");
        let g = group(Conjunction::And, vec![cond(0, FilterOp::Regex, "(")]);
        assert!(matching_rows(&d, &g).is_err());
    }
}
