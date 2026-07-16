//! Cross-column validation rules (F27): relationships BETWEEN columns,
//! extending the per-column profile validation (expected types, regex,
//! ranges) that already exists. Rules are a closed, validated DTO set — no
//! arbitrary expressions — referencing columns by NAME so they can live in
//! reusable file profiles. Scanning is read-only, cancellable, and
//! revision-stamped; numeric and date comparisons use typed coercion via
//! [`crate::analyze`], never lexical comparison.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::analyze;
use crate::document::Document;
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;

/// Violations sampled per rule for the report (full counts still reported).
const SAMPLE_LIMIT: usize = 100;
const ROW_CHUNK: usize = 4096;

/// Numeric comparison operators (closed set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompareOp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

impl CompareOp {
    fn eval(self, a: f64, b: f64) -> bool {
        match self {
            CompareOp::Lt => a < b,
            CompareOp::Le => a <= b,
            CompareOp::Gt => a > b,
            CompareOp::Ge => a >= b,
            CompareOp::Eq => a == b,
            CompareOp::Ne => a != b,
        }
    }

    fn label(self) -> &'static str {
        match self {
            CompareOp::Lt => "<",
            CompareOp::Le => "≤",
            CompareOp::Gt => ">",
            CompareOp::Ge => "≥",
            CompareOp::Eq => "=",
            CompareOp::Ne => "≠",
        }
    }
}

/// Condition on the "when" column of a conditional-required rule. Blank
/// condition values are handled EXPLICITLY: `blank` and `nonBlank` are their
/// own variants, and `equals` never matches a blank cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WhenCondition {
    Equals { value: String },
    NonBlank,
    Blank,
}

/// The closed set of cross-column rules. Columns are referenced by NAME so
/// rules persist meaningfully in file profiles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CrossRule {
    /// Trimmed string equality between two columns (`negate` = must differ).
    ColumnsEqual {
        left: String,
        right: String,
        #[serde(default)]
        negate: bool,
    },
    /// Typed numeric comparison; blank on either side skips the row, a
    /// non-blank value that cannot parse as a number is a violation.
    NumericCompare {
        left: String,
        op: CompareOp,
        right: String,
    },
    /// `earlier` must be before `later` (typed date coercion; same blank /
    /// unparseable policy as numeric comparison).
    DateOrder {
        earlier: String,
        later: String,
        #[serde(default)]
        allow_equal: bool,
    },
    /// When the condition holds, `then_required` must be non-blank.
    ConditionalRequired {
        when_column: String,
        when: WhenCondition,
        then_required: String,
    },
    /// Exactly one of the columns is populated (non-blank).
    ExactlyOne { columns: Vec<String> },
    /// At least one of the columns is populated.
    AtLeastOne { columns: Vec<String> },
    /// Mutually exclusive: at most one of the columns is populated.
    AtMostOne { columns: Vec<String> },
    /// The parts must sum to the total within a tolerance (absolute, or a
    /// percentage of the total when `tolerance_percent`). Blank parts count
    /// as zero; an all-blank row is skipped.
    SumEquals {
        parts: Vec<String>,
        total: String,
        tolerance: f64,
        #[serde(default)]
        tolerance_percent: bool,
    },
    /// The tuple of trimmed values must be one of the allowed combinations.
    /// Rows where every referenced cell is blank are skipped.
    AllowedCombinations {
        columns: Vec<String>,
        allowed: Vec<Vec<String>>,
    },
}

impl CrossRule {
    /// Column names this rule reads, in display order.
    pub fn columns(&self) -> Vec<&str> {
        match self {
            CrossRule::ColumnsEqual { left, right, .. } => vec![left, right],
            CrossRule::NumericCompare { left, right, .. } => vec![left, right],
            CrossRule::DateOrder { earlier, later, .. } => vec![earlier, later],
            CrossRule::ConditionalRequired {
                when_column,
                then_required,
                ..
            } => vec![when_column, then_required],
            CrossRule::ExactlyOne { columns }
            | CrossRule::AtLeastOne { columns }
            | CrossRule::AtMostOne { columns }
            | CrossRule::AllowedCombinations { columns, .. } => {
                columns.iter().map(String::as_str).collect()
            }
            CrossRule::SumEquals { parts, total, .. } => {
                let mut v: Vec<&str> = parts.iter().map(String::as_str).collect();
                v.push(total);
                v
            }
        }
    }

    /// Human-readable one-line summary for the UI.
    pub fn describe(&self) -> String {
        match self {
            CrossRule::ColumnsEqual {
                left,
                right,
                negate,
            } => format!(
                "\"{left}\" must {} \"{right}\"",
                if *negate { "differ from" } else { "equal" }
            ),
            CrossRule::NumericCompare { left, op, right } => {
                format!("\"{left}\" {} \"{right}\" (numeric)", op.label())
            }
            CrossRule::DateOrder {
                earlier,
                later,
                allow_equal,
            } => format!(
                "\"{earlier}\" must be {} \"{later}\"",
                if *allow_equal {
                    "on or before"
                } else {
                    "before"
                }
            ),
            CrossRule::ConditionalRequired {
                when_column,
                when,
                then_required,
            } => {
                let cond = match when {
                    WhenCondition::Equals { value } => format!("= \"{value}\""),
                    WhenCondition::NonBlank => "is not blank".to_string(),
                    WhenCondition::Blank => "is blank".to_string(),
                };
                format!("when \"{when_column}\" {cond}, \"{then_required}\" is required")
            }
            CrossRule::ExactlyOne { columns } => {
                format!("exactly one of {} populated", columns.join(", "))
            }
            CrossRule::AtLeastOne { columns } => {
                format!("at least one of {} populated", columns.join(", "))
            }
            CrossRule::AtMostOne { columns } => {
                format!("at most one of {} populated", columns.join(", "))
            }
            CrossRule::SumEquals {
                parts,
                total,
                tolerance,
                tolerance_percent,
            } => format!(
                "{} must sum to \"{total}\" (±{tolerance}{})",
                parts.join(" + "),
                if *tolerance_percent { "%" } else { "" }
            ),
            CrossRule::AllowedCombinations { columns, allowed } => format!(
                "({}) must be one of {} allowed combination{}",
                columns.join(", "),
                allowed.len(),
                if allowed.len() == 1 { "" } else { "s" }
            ),
        }
    }
}

/// Structural validation, applied BEFORE any scanning. Checks shape only;
/// column-name resolution happens against the document separately.
pub fn validate_rules(rules: &[CrossRule]) -> AppResult<()> {
    if rules.is_empty() {
        return Err(AppError::invalid("add at least one rule"));
    }
    for rule in rules {
        match rule {
            CrossRule::ExactlyOne { columns }
            | CrossRule::AtLeastOne { columns }
            | CrossRule::AtMostOne { columns } => {
                if columns.len() < 2 {
                    return Err(AppError::invalid(
                        "populated-count rules need at least two columns",
                    ));
                }
            }
            CrossRule::SumEquals {
                parts, tolerance, ..
            } => {
                if parts.is_empty() {
                    return Err(AppError::invalid("sum rule needs at least one part column"));
                }
                if !tolerance.is_finite() || *tolerance < 0.0 {
                    return Err(AppError::invalid("tolerance must be a non-negative number"));
                }
            }
            CrossRule::AllowedCombinations { columns, allowed } => {
                if columns.is_empty() {
                    return Err(AppError::invalid("combination rule needs columns"));
                }
                if allowed.is_empty() {
                    return Err(AppError::invalid(
                        "combination rule needs at least one allowed combination",
                    ));
                }
                if allowed.iter().any(|row| row.len() != columns.len()) {
                    return Err(AppError::invalid(
                        "every allowed combination must list one value per column",
                    ));
                }
            }
            CrossRule::ColumnsEqual { left, right, .. }
            | CrossRule::NumericCompare { left, right, .. } => {
                if left == right {
                    return Err(AppError::invalid(
                        "comparison rules need two different columns",
                    ));
                }
            }
            CrossRule::DateOrder { earlier, later, .. } => {
                if earlier == later {
                    return Err(AppError::invalid(
                        "date-order rules need two different columns",
                    ));
                }
            }
            CrossRule::ConditionalRequired { .. } => {}
        }
    }
    Ok(())
}

/// Resolve every referenced column name to its (first) index; a missing
/// column is a configuration error, reported before scanning.
fn resolve(doc: &Document, rules: &[CrossRule]) -> AppResult<HashMap<String, usize>> {
    let mut map = HashMap::new();
    for rule in rules {
        for name in rule.columns() {
            if map.contains_key(name) {
                continue;
            }
            match doc.headers().iter().position(|h| h == name) {
                Some(i) => {
                    map.insert(name.to_string(), i);
                }
                None => {
                    return Err(AppError::invalid(format!(
                        "rule references missing column \"{name}\""
                    )));
                }
            }
        }
    }
    Ok(map)
}

/// One rule's verdict on one row: `None` = passes or not applicable.
fn violation_reason(rule: &CrossRule, cell: &dyn Fn(&str) -> String) -> Option<String> {
    let blank = |name: &str| cell(name).trim().is_empty();
    let number = |name: &str| -> Result<Option<f64>, String> {
        let raw = cell(name);
        let t = raw.trim();
        if t.is_empty() {
            return Ok(None);
        }
        analyze::as_number(t)
            .map(Some)
            .ok_or_else(|| format!("\"{name}\" is not numeric ({t})"))
    };

    match rule {
        CrossRule::ColumnsEqual {
            left,
            right,
            negate,
        } => {
            let equal = cell(left).trim() == cell(right).trim();
            if equal == *negate {
                Some(if *negate {
                    format!("\"{left}\" equals \"{right}\"")
                } else {
                    format!("\"{left}\" differs from \"{right}\"")
                })
            } else {
                None
            }
        }
        CrossRule::NumericCompare { left, op, right } => {
            let (a, b) = match (number(left), number(right)) {
                (Err(e), _) | (_, Err(e)) => return Some(e),
                (Ok(a), Ok(b)) => (a?, b?), // blank on either side: skip
            };
            if op.eval(a, b) {
                None
            } else {
                Some(format!("{a} {} {b} fails", op.label()))
            }
        }
        CrossRule::DateOrder {
            earlier,
            later,
            allow_equal,
        } => {
            let parse = |name: &str| -> Result<Option<chrono::NaiveDateTime>, String> {
                let raw = cell(name);
                let t = raw.trim();
                if t.is_empty() {
                    return Ok(None);
                }
                analyze::parse_date(t)
                    .map(Some)
                    .ok_or_else(|| format!("\"{name}\" is not a date ({t})"))
            };
            let (a, b) = match (parse(earlier), parse(later)) {
                (Err(e), _) | (_, Err(e)) => return Some(e),
                (Ok(a), Ok(b)) => (a?, b?),
            };
            let ok = if *allow_equal { a <= b } else { a < b };
            if ok {
                None
            } else {
                Some(format!("\"{earlier}\" is not before \"{later}\""))
            }
        }
        CrossRule::ConditionalRequired {
            when_column,
            when,
            then_required,
        } => {
            let raw = cell(when_column);
            let value = raw.trim();
            let fires = match when {
                WhenCondition::Equals { value: expect } => {
                    !value.is_empty() && value == expect.trim()
                }
                WhenCondition::NonBlank => !value.is_empty(),
                WhenCondition::Blank => value.is_empty(),
            };
            if fires && blank(then_required) {
                Some(format!("\"{then_required}\" is required here but blank"))
            } else {
                None
            }
        }
        CrossRule::ExactlyOne { columns } => {
            let n = columns.iter().filter(|c| !blank(c)).count();
            (n != 1).then(|| format!("{n} of {} populated (need exactly 1)", columns.len()))
        }
        CrossRule::AtLeastOne { columns } => {
            let n = columns.iter().filter(|c| !blank(c)).count();
            (n == 0).then(|| "none populated (need at least 1)".to_string())
        }
        CrossRule::AtMostOne { columns } => {
            let n = columns.iter().filter(|c| !blank(c)).count();
            (n > 1).then(|| format!("{n} populated (mutually exclusive)"))
        }
        CrossRule::SumEquals {
            parts,
            total,
            tolerance,
            tolerance_percent,
        } => {
            let mut sum = 0.0;
            let mut any_part = false;
            for part in parts {
                match number(part) {
                    Err(e) => return Some(e),
                    Ok(Some(v)) => {
                        sum += v;
                        any_part = true;
                    }
                    Ok(None) => {} // blank part counts as 0
                }
            }
            let total_value = match number(total) {
                Err(e) => return Some(e),
                Ok(v) => v,
            };
            match total_value {
                None if !any_part => None, // fully blank row: skip
                None => Some(format!("\"{total}\" is blank but parts sum to {sum}")),
                Some(t) => {
                    let allowed = if *tolerance_percent {
                        t.abs() * tolerance / 100.0
                    } else {
                        *tolerance
                    };
                    ((sum - t).abs() > allowed)
                        .then(|| format!("parts sum to {sum}, total is {t} (±{allowed} allowed)"))
                }
            }
        }
        CrossRule::AllowedCombinations { columns, allowed } => {
            let tuple: Vec<String> = columns.iter().map(|c| cell(c).trim().to_string()).collect();
            if tuple.iter().all(String::is_empty) {
                return None;
            }
            let ok = allowed.iter().any(|row| {
                row.iter()
                    .map(|v| v.trim())
                    .eq(tuple.iter().map(String::as_str))
            });
            (!ok).then(|| format!("({}) is not an allowed combination", tuple.join(", ")))
        }
    }
}

/// One sampled violation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Violation {
    /// Absolute row index.
    pub row: usize,
    /// (column name, value) for the rule's referenced columns.
    pub values: Vec<(String, String)>,
    pub reason: String,
}

/// Per-rule outcome.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleViolations {
    /// Index into the submitted rule list.
    pub rule: usize,
    pub description: String,
    pub violations: usize,
    /// First violations, bounded to [`SAMPLE_LIMIT`].
    pub sample: Vec<Violation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossValReport {
    pub revision: u64,
    pub scanned_rows: usize,
    /// Sum of violations across rules (a row can violate several).
    pub total_violations: usize,
    /// Distinct rows violating at least one rule.
    pub violating_rows: usize,
    pub rules: Vec<RuleViolations>,
}

/// Run all rules in ONE pass over the document. Read-only.
pub fn scan(doc: &Document, rules: &[CrossRule], ctx: &JobCtx) -> AppResult<CrossValReport> {
    validate_rules(rules)?;
    let index = resolve(doc, rules)?;
    ctx.set_total(doc.n_rows() as u64);

    let mut results: Vec<RuleViolations> = rules
        .iter()
        .enumerate()
        .map(|(i, r)| RuleViolations {
            rule: i,
            description: r.describe(),
            violations: 0,
            sample: Vec::new(),
        })
        .collect();
    let mut violating_rows: HashSet<usize> = HashSet::new();

    let mut pending = 0u64;
    doc.visit_rows(0..doc.n_rows(), &mut |i, row| {
        let cell = |name: &str| -> String {
            index
                .get(name)
                .and_then(|&c| row.get(c))
                .cloned()
                .unwrap_or_default()
        };
        for (rule, result) in rules.iter().zip(results.iter_mut()) {
            if let Some(reason) = violation_reason(rule, &cell) {
                result.violations += 1;
                violating_rows.insert(i);
                if result.sample.len() < SAMPLE_LIMIT {
                    result.sample.push(Violation {
                        row: i,
                        values: rule
                            .columns()
                            .iter()
                            .map(|name| (name.to_string(), cell(name)))
                            .collect(),
                        reason,
                    });
                }
            }
        }
        pending += 1;
        if pending >= ROW_CHUNK as u64 {
            ctx.advance(pending)?;
            pending = 0;
        }
        Ok(true)
    })?;
    ctx.advance(pending)?;

    Ok(CrossValReport {
        revision: doc.revision(),
        scanned_rows: doc.n_rows(),
        total_violations: results.iter().map(|r| r.violations).sum(),
        violating_rows: violating_rows.len(),
        rules: results,
    })
}

/// Absolute rows violating one rule (`Some(index)`) or any rule (`None`),
/// recomputed for the filter action under the caller's revision guard.
pub fn violating_rows(
    doc: &Document,
    rules: &[CrossRule],
    rule: Option<usize>,
) -> AppResult<Vec<usize>> {
    validate_rules(rules)?;
    let selected: Vec<&CrossRule> = match rule {
        Some(i) => vec![rules
            .get(i)
            .ok_or_else(|| AppError::invalid("rule index out of range"))?],
        None => rules.iter().collect(),
    };
    let index = resolve(doc, rules)?;
    let mut out = Vec::new();
    doc.visit_rows(0..doc.n_rows(), &mut |i, row| {
        let cell = |name: &str| -> String {
            index
                .get(name)
                .and_then(|&c| row.get(c))
                .cloned()
                .unwrap_or_default()
        };
        if selected
            .iter()
            .any(|r| violation_reason(r, &cell).is_some())
        {
            out.push(i);
        }
        Ok(true)
    })?;
    Ok(out)
}

/// A cached scan outcome: the rules that ran and the report they produced.
pub type CachedCrossVal = (Vec<CrossRule>, CrossValReport);

/// Last completed cross-validation report + the rules it ran, per document.
#[derive(Default)]
pub struct CrossValCache(Arc<Mutex<HashMap<u64, CachedCrossVal>>>);

impl CrossValCache {
    pub fn share(&self) -> Arc<Mutex<HashMap<u64, CachedCrossVal>>> {
        Arc::clone(&self.0)
    }

    pub fn get(&self, doc_id: u64) -> Option<(Vec<CrossRule>, CrossValReport)> {
        self.0.lock().ok()?.get(&doc_id).cloned()
    }

    pub fn remove(&self, doc_id: u64) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(&doc_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;
    use crate::parse::{parse, ParseSettings};

    fn doc(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn run(doc: &Document, rules: Vec<CrossRule>) -> CrossValReport {
        let registry = JobRegistry::default();
        let ctx = registry.begin("crossval", None, |_| {});
        scan(doc, &rules, &ctx).unwrap()
    }

    #[test]
    fn columns_equal_and_differ() {
        let d = doc("a,b\nx,x\nx,y\n,\n");
        let equal = run(
            &d,
            vec![CrossRule::ColumnsEqual {
                left: "a".into(),
                right: "b".into(),
                negate: false,
            }],
        );
        assert_eq!(equal.rules[0].violations, 1);
        assert_eq!(equal.rules[0].sample[0].row, 1);

        let differ = run(
            &d,
            vec![CrossRule::ColumnsEqual {
                left: "a".into(),
                right: "b".into(),
                negate: true,
            }],
        );
        // Row 0 (x=x) and row 2 (blank=blank) both violate "must differ".
        assert_eq!(differ.rules[0].violations, 2);
    }

    #[test]
    fn numeric_compare_uses_typed_coercion_not_lexical() {
        // Lexically "9.5" > "10", numerically it is not.
        let d = doc("low,high\n9.5,10\n11,10\n,10\nabc,10\n");
        let r = run(
            &d,
            vec![CrossRule::NumericCompare {
                left: "low".into(),
                op: CompareOp::Le,
                right: "high".into(),
            }],
        );
        // Row 1 (11 > 10) violates; row 2 (blank) skips; row 3 unparseable.
        assert_eq!(r.rules[0].violations, 2);
        assert!(r.rules[0].sample[1].reason.contains("not numeric"));
    }

    #[test]
    fn date_order_is_typed() {
        let d =
            doc("start,end\n2024-01-02,2024-01-10\n2024-02-01,2024-01-01\n2024-03-01,2024-03-01\n");
        let strict = run(
            &d,
            vec![CrossRule::DateOrder {
                earlier: "start".into(),
                later: "end".into(),
                allow_equal: false,
            }],
        );
        assert_eq!(strict.rules[0].violations, 2); // reversed + equal
        let loose = run(
            &d,
            vec![CrossRule::DateOrder {
                earlier: "start".into(),
                later: "end".into(),
                allow_equal: true,
            }],
        );
        assert_eq!(loose.rules[0].violations, 1); // only the reversed pair
    }

    #[test]
    fn conditional_required_handles_blank_conditions_explicitly() {
        let d = doc("status,reason\nrejected,too slow\nrejected,\napproved,\n,\n");
        let equals = run(
            &d,
            vec![CrossRule::ConditionalRequired {
                when_column: "status".into(),
                when: WhenCondition::Equals {
                    value: "rejected".into(),
                },
                then_required: "reason".into(),
            }],
        );
        // Only row 1 fires AND is blank; blank status (row 3) never matches Equals.
        assert_eq!(equals.rules[0].violations, 1);
        assert_eq!(equals.rules[0].sample[0].row, 1);

        let when_blank = run(
            &d,
            vec![CrossRule::ConditionalRequired {
                when_column: "status".into(),
                when: WhenCondition::Blank,
                then_required: "reason".into(),
            }],
        );
        assert_eq!(when_blank.rules[0].violations, 1); // row 3
        assert_eq!(when_blank.rules[0].sample[0].row, 3);
    }

    #[test]
    fn populated_count_rules() {
        let d = doc("a,b,c\nx,,\nx,y,\n,,\nx,y,z\n");
        let rules = vec![
            CrossRule::ExactlyOne {
                columns: vec!["a".into(), "b".into(), "c".into()],
            },
            CrossRule::AtLeastOne {
                columns: vec!["a".into(), "b".into(), "c".into()],
            },
            CrossRule::AtMostOne {
                columns: vec!["a".into(), "b".into(), "c".into()],
            },
        ];
        let r = run(&d, rules);
        assert_eq!(r.rules[0].violations, 3); // rows 1 (2), 2 (0), 3 (3)
        assert_eq!(r.rules[1].violations, 1); // row 2
        assert_eq!(r.rules[2].violations, 2); // rows 1 and 3
        assert_eq!(r.violating_rows, 3); // distinct rows 1, 2, 3
        assert_eq!(r.total_violations, 6);
    }

    #[test]
    fn sum_equality_supports_absolute_and_percent_tolerance() {
        let d = doc("net,tax,gross\n100,20,120\n100,20,121\n100,20,140\n,,\n");
        let absolute = run(
            &d,
            vec![CrossRule::SumEquals {
                parts: vec!["net".into(), "tax".into()],
                total: "gross".into(),
                tolerance: 1.0,
                tolerance_percent: false,
            }],
        );
        // 120 exact ok; 121 within ±1; 140 out; blank row skipped.
        assert_eq!(absolute.rules[0].violations, 1);

        let percent = run(
            &d,
            vec![CrossRule::SumEquals {
                parts: vec!["net".into(), "tax".into()],
                total: "gross".into(),
                tolerance: 20.0,
                tolerance_percent: true,
            }],
        );
        // ±20% of 140 = 28 ≥ |120-140|: within; ±20% of 121 covers too.
        assert_eq!(percent.rules[0].violations, 0);
    }

    #[test]
    fn allowed_combinations() {
        let d = doc("country,currency\nUS,USD\nUS,EUR\nDE,EUR\n,\n");
        let r = run(
            &d,
            vec![CrossRule::AllowedCombinations {
                columns: vec!["country".into(), "currency".into()],
                allowed: vec![
                    vec!["US".into(), "USD".into()],
                    vec!["DE".into(), "EUR".into()],
                ],
            }],
        );
        assert_eq!(r.rules[0].violations, 1);
        assert_eq!(r.rules[0].sample[0].row, 1);
        assert!(r.rules[0].sample[0].reason.contains("US, EUR"));
    }

    #[test]
    fn invalid_configurations_are_rejected_before_scanning() {
        let d = doc("a,b\n1,2\n");
        let cases: Vec<CrossRule> = vec![
            CrossRule::ExactlyOne {
                columns: vec!["a".into()],
            },
            CrossRule::SumEquals {
                parts: vec![],
                total: "b".into(),
                tolerance: 0.0,
                tolerance_percent: false,
            },
            CrossRule::SumEquals {
                parts: vec!["a".into()],
                total: "b".into(),
                tolerance: -1.0,
                tolerance_percent: false,
            },
            CrossRule::AllowedCombinations {
                columns: vec!["a".into()],
                allowed: vec![vec!["x".into(), "y".into()]],
            },
            CrossRule::ColumnsEqual {
                left: "a".into(),
                right: "a".into(),
                negate: false,
            },
        ];
        for rule in cases {
            assert!(
                validate_rules(std::slice::from_ref(&rule)).is_err(),
                "{rule:?}"
            );
        }
        // A missing column is caught at resolution, also before scanning.
        let registry = JobRegistry::default();
        let ctx = registry.begin("crossval", None, |_| {});
        let missing = CrossRule::ColumnsEqual {
            left: "a".into(),
            right: "nope".into(),
            negate: false,
        };
        assert!(scan(&d, &[missing], &ctx).is_err());
    }

    #[test]
    fn filter_rows_for_one_rule_or_all() {
        let d = doc("a,b\nx,x\nx,y\n1,2\n");
        let rules = vec![
            CrossRule::ColumnsEqual {
                left: "a".into(),
                right: "b".into(),
                negate: false,
            },
            CrossRule::NumericCompare {
                left: "a".into(),
                op: CompareOp::Ge,
                right: "b".into(),
            },
        ];
        let one = violating_rows(&d, &rules, Some(0)).unwrap();
        assert_eq!(one, vec![1, 2]); // "x"≠"y" and "1"≠"2"
                                     // Row 0/1 are non-numeric (violate rule 2); row 2 has 1 < 2.
        let all = violating_rows(&d, &rules, None).unwrap();
        assert_eq!(all, vec![0, 1, 2]);
        assert!(violating_rows(&d, &rules, Some(9)).is_err());
    }

    #[test]
    fn reports_are_deterministic_and_bounded() {
        let mut csv = String::from("a,b\n");
        for _ in 0..300 {
            csv.push_str("x,y\n");
        }
        let d = doc(&csv);
        let rule = vec![CrossRule::ColumnsEqual {
            left: "a".into(),
            right: "b".into(),
            negate: false,
        }];
        let r1 = run(&d, rule.clone());
        let r2 = run(&d, rule);
        assert_eq!(r1.rules[0].violations, 300);
        assert_eq!(r1.rules[0].sample.len(), SAMPLE_LIMIT);
        assert_eq!(
            serde_json::to_string(&r1).unwrap(),
            serde_json::to_string(&r2).unwrap()
        );
    }
}
