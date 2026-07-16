//! PII detection and redaction (F28): find common sensitive identifiers and
//! remove them under explicit user control. Deterministic detectors only —
//! this feature does NOT claim to find names, street addresses, or all
//! possible PII. Security posture: full card and SSN values are NEVER
//! carried in reports or previews (samples and examples are masked), the
//! pseudonymization secret is provided per call and never persisted, salts
//! come from the OS CSPRNG, every mutation is previewed and applies as one
//! undo step, and nothing leaves the device (the audit log stores counts,
//! never values).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use hmac::{Hmac, Mac};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::document::Document;
use crate::dto::ExportScope;
use crate::error::{AppError, AppResult};
use crate::export_scope::resolve_scope;
use crate::job::JobCtx;
use crate::semantic::{self, SemanticType};

/// Masked samples carried per finding.
const SAMPLE_LIMIT: usize = 3;
const EXAMPLE_LIMIT: usize = 10;
const ROW_CHUNK: usize = 4096;

/// The deterministic detector set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PiiDetector {
    Email,
    PhoneNumber,
    IpAddress,
    /// US Social Security number pattern (dashed form, invalid areas
    /// excluded). A pattern match, not identity verification.
    Ssn,
    /// Payment-card CANDIDATE: 13–19 digits with optional separators that
    /// pass the Luhn checksum. Luhn-invalid numbers are never classified.
    PaymentCard,
    /// A user-provided regex (validated before scanning).
    Custom {
        name: String,
        pattern: String,
    },
}

impl PiiDetector {
    pub fn label(&self) -> String {
        match self {
            PiiDetector::Email => "email".into(),
            PiiDetector::PhoneNumber => "phone number".into(),
            PiiDetector::IpAddress => "IP address".into(),
            PiiDetector::Ssn => "SSN pattern".into(),
            PiiDetector::PaymentCard => "payment card (Luhn)".into(),
            PiiDetector::Custom { name, .. } => format!("custom: {name}"),
        }
    }

    fn validation(&self) -> &'static str {
        match self {
            PiiDetector::PaymentCard => "pattern + Luhn checksum",
            _ => "pattern",
        }
    }
}

fn ssn_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\d{3})-(\d{2})-(\d{4})$").expect("static regex"))
}

/// Whether a trimmed cell matches the SSN pattern (invalid areas excluded).
fn is_ssn(value: &str) -> bool {
    let Some(caps) = ssn_regex().captures(value) else {
        return false;
    };
    let area: u32 = caps[1].parse().unwrap_or(0);
    let group: u32 = caps[2].parse().unwrap_or(0);
    let serial: u32 = caps[3].parse().unwrap_or(0);
    area != 0 && area != 666 && area < 900 && group != 0 && serial != 0
}

/// Luhn checksum over the digits of a card-like value.
fn luhn_valid(digits: &[u32]) -> bool {
    let mut sum = 0u32;
    for (i, &d) in digits.iter().rev().enumerate() {
        let mut d = d;
        if !i.is_multiple_of(2) {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum.is_multiple_of(10)
}

/// Whether a trimmed cell is a Luhn-valid payment-card candidate.
fn is_payment_card(value: &str) -> bool {
    if !value
        .chars()
        .all(|c| c.is_ascii_digit() || c == ' ' || c == '-')
    {
        return false;
    }
    let digits: Vec<u32> = value.chars().filter_map(|c| c.to_digit(10)).collect();
    (13..=19).contains(&digits.len()) && luhn_valid(&digits)
}

/// Match one trimmed, non-blank cell against a detector.
fn matches_detector(value: &str, detector: &PiiDetector, custom: Option<&Regex>) -> bool {
    match detector {
        PiiDetector::Email => semantic::matches_type(value, SemanticType::Email),
        PiiDetector::PhoneNumber => semantic::matches_type(value, SemanticType::PhoneNumber),
        PiiDetector::IpAddress => {
            semantic::matches_type(value, SemanticType::Ipv4)
                || semantic::matches_type(value, SemanticType::Ipv6)
        }
        PiiDetector::Ssn => is_ssn(value),
        PiiDetector::PaymentCard => is_payment_card(value),
        PiiDetector::Custom { .. } => custom
            .map(|re| {
                re.find(value)
                    .map(|m| m.start() == 0 && m.end() == value.len())
                    .unwrap_or(false)
            })
            .unwrap_or(false),
    }
}

/// Mask a detected value for display: only the LAST four characters stay
/// visible (fewer for short values), everything else becomes bullets.
/// Emails additionally keep their domain. Full card/SSN values can never
/// appear anywhere in the UI.
pub fn mask_value(value: &str, detector: &PiiDetector) -> String {
    let value = value.trim();
    if let PiiDetector::Email = detector {
        if let Some(at) = value.rfind('@') {
            return format!("•••{}", &value[at..]);
        }
    }
    let chars: Vec<char> = value.chars().collect();
    let visible = chars.len().min(4).min(chars.len().saturating_sub(1).max(1));
    let masked = "•".repeat(chars.len().saturating_sub(visible));
    let tail: String = chars[chars.len() - visible..].iter().collect();
    format!("{masked}{tail}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiiSpec {
    pub detectors: Vec<PiiDetector>,
    pub scope: ExportScope,
}

fn compile_customs(detectors: &[PiiDetector]) -> AppResult<HashMap<usize, Regex>> {
    let mut map = HashMap::new();
    for (i, d) in detectors.iter().enumerate() {
        if let PiiDetector::Custom { pattern, .. } = d {
            let re = Regex::new(pattern)
                .map_err(|e| AppError::invalid(format!("invalid custom pattern: {e}")))?;
            map.insert(i, re);
        }
    }
    Ok(map)
}

fn validate(doc: &Document, spec: &PiiSpec) -> AppResult<()> {
    if spec.detectors.is_empty() {
        return Err(AppError::invalid("pick at least one detector"));
    }
    let _ = doc;
    Ok(())
}

/// One (detector, column) finding.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PiiFinding {
    /// Index into the submitted detector list.
    pub detector: usize,
    pub detector_label: String,
    pub validation: String,
    pub column: usize,
    pub count: usize,
    /// MASKED samples only — raw values never leave the scan.
    pub samples: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PiiReport {
    pub revision: u64,
    pub scanned_rows: usize,
    pub total_matches: usize,
    pub findings: Vec<PiiFinding>,
}

/// Run a PII scan. Read-only; never dirties the document.
pub fn scan(doc: &Document, spec: &PiiSpec, ctx: &JobCtx) -> AppResult<PiiReport> {
    validate(doc, spec)?;
    let customs = compile_customs(&spec.detectors)?;
    let rows = resolve_scope(doc, &spec.scope)?.rows;
    ctx.set_total(rows.len() as u64);

    let mut counts: HashMap<(usize, usize), (usize, Vec<String>)> = HashMap::new();
    let mut pending = 0u64;
    doc.visit_rows_at(&rows, &mut |_, row| {
        for (c, cell) in row.iter().enumerate() {
            let trimmed = cell.trim();
            if trimmed.is_empty() {
                continue;
            }
            for (d, detector) in spec.detectors.iter().enumerate() {
                if matches_detector(trimmed, detector, customs.get(&d)) {
                    let entry = counts.entry((d, c)).or_insert_with(|| (0, Vec::new()));
                    entry.0 += 1;
                    if entry.1.len() < SAMPLE_LIMIT {
                        entry.1.push(mask_value(trimmed, detector));
                    }
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

    let mut findings: Vec<PiiFinding> = counts
        .into_iter()
        .map(|((d, c), (count, samples))| PiiFinding {
            detector: d,
            detector_label: spec.detectors[d].label(),
            validation: spec.detectors[d].validation().to_string(),
            column: c,
            count,
            samples,
        })
        .collect();
    findings.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.column.cmp(&b.column))
            .then_with(|| a.detector.cmp(&b.detector))
    });
    Ok(PiiReport {
        revision: doc.revision(),
        scanned_rows: rows.len(),
        total_matches: findings.iter().map(|f| f.count).sum(),
        findings,
    })
}

/// Redaction actions — all previewed, all one undo step. False positives
/// are expected: actions apply only to the explicitly selected
/// (detector, column) finding, never blanket.
#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RedactionAction {
    /// Replace matched values with a fixed string.
    FixedReplacement { replacement: String },
    /// Keep the last N characters, mask the rest.
    KeepLast { n: usize },
    /// Mask everything.
    FullMask,
    /// Replace with a keyed pseudonym: HMAC-SHA-256(secret ‖ salt, value),
    /// hex-truncated. The secret is NEVER persisted; the salt is generated
    /// from the OS CSPRNG when absent and echoed back for reuse.
    Pseudonymize {
        secret: String,
        #[serde(default)]
        salt: Option<String>,
    },
    /// Delete the whole column.
    RemoveColumn,
    /// Delete the rows containing matches.
    RemoveRows,
}

/// 16 CSPRNG bytes as hex.
fn generate_salt() -> AppResult<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| AppError::Other(format!("secure random unavailable: {e}")))?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Deterministic keyed pseudonym for one value.
pub fn pseudonym(secret: &str, salt: &str, value: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(format!("{secret}\u{1f}{salt}").as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(value.as_bytes());
    let digest = mac.finalize().into_bytes();
    format!("pii_{}", &hex(&digest)[..16])
}

/// What a redaction would do, with MASKED before-values in the examples.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactionPreview {
    pub revision: u64,
    pub cells_affected: usize,
    pub rows_removed: usize,
    pub column_removed: bool,
    /// (masked before, after) pairs.
    pub examples: Vec<(String, String)>,
    /// The salt used for pseudonymization (generated when not supplied) —
    /// reuse it to keep pseudonyms consistent across runs.
    pub salt: Option<String>,
}

pub struct RedactionComputed {
    pub changes: Vec<(usize, usize, String)>,
    pub remove_rows: Vec<usize>,
    pub remove_column: Option<usize>,
    pub preview: RedactionPreview,
}

/// Compute a redaction for ONE (detector, column) finding.
pub fn redaction_changes(
    doc: &Document,
    spec: &PiiSpec,
    detector: usize,
    column: usize,
    action: &RedactionAction,
) -> AppResult<RedactionComputed> {
    validate(doc, spec)?;
    let det = spec
        .detectors
        .get(detector)
        .ok_or_else(|| AppError::invalid("detector index out of range"))?;
    if column >= doc.n_cols() {
        return Err(AppError::invalid("column out of range"));
    }
    if let RedactionAction::KeepLast { n } = action {
        if *n == 0 || *n > 32 {
            return Err(AppError::invalid("keep between 1 and 32 characters"));
        }
    }
    if let RedactionAction::Pseudonymize { secret, .. } = action {
        if secret.is_empty() {
            return Err(AppError::invalid("the pseudonymization secret is required"));
        }
    }
    let customs = compile_customs(&spec.detectors)?;
    let custom = customs.get(&detector);
    let rows = resolve_scope(doc, &spec.scope)?.rows;

    // Resolve the salt once so every value in this run shares it.
    let salt = match action {
        RedactionAction::Pseudonymize { salt, .. } => Some(match salt {
            Some(s) if !s.is_empty() => s.clone(),
            _ => generate_salt()?,
        }),
        _ => None,
    };

    let mut changes: Vec<(usize, usize, String)> = Vec::new();
    let mut remove_rows: Vec<usize> = Vec::new();
    let mut examples: Vec<(String, String)> = Vec::new();
    let remove_column = matches!(action, RedactionAction::RemoveColumn).then_some(column);

    doc.visit_rows_at(&rows, &mut |r, row| {
        let cell = row.get(column).map(String::as_str).unwrap_or("");
        let trimmed = cell.trim();
        if trimmed.is_empty() || !matches_detector(trimmed, det, custom) {
            return Ok(true);
        }
        match action {
            RedactionAction::RemoveColumn => {} // handled structurally
            RedactionAction::RemoveRows => remove_rows.push(r),
            _ => {
                let after = match action {
                    RedactionAction::FixedReplacement { replacement } => replacement.clone(),
                    RedactionAction::KeepLast { n } => {
                        let chars: Vec<char> = trimmed.chars().collect();
                        let keep = (*n).min(chars.len());
                        let tail: String = chars[chars.len() - keep..].iter().collect();
                        format!("{}{tail}", "•".repeat(chars.len() - keep))
                    }
                    RedactionAction::FullMask => "•".repeat(trimmed.chars().count()),
                    RedactionAction::Pseudonymize { secret, .. } => {
                        pseudonym(secret, salt.as_deref().unwrap_or_default(), trimmed)
                    }
                    _ => unreachable!("structural actions handled above"),
                };
                if examples.len() < EXAMPLE_LIMIT {
                    examples.push((mask_value(trimmed, det), after.clone()));
                }
                changes.push((r, column, after));
            }
        }
        Ok(true)
    })?;

    let preview = RedactionPreview {
        revision: doc.revision(),
        cells_affected: changes.len(),
        rows_removed: remove_rows.len(),
        column_removed: remove_column.is_some(),
        examples,
        salt,
    };
    Ok(RedactionComputed {
        changes,
        remove_rows,
        remove_column,
        preview,
    })
}

/// Last completed PII report + the spec that produced it, per document.
pub type CachedPii = (PiiSpec, PiiReport);

#[derive(Default)]
pub struct PiiCache(Arc<Mutex<HashMap<u64, CachedPii>>>);

impl PiiCache {
    pub fn share(&self) -> Arc<Mutex<HashMap<u64, CachedPii>>> {
        Arc::clone(&self.0)
    }

    pub fn get(&self, doc_id: u64) -> Option<CachedPii> {
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

    fn spec(detectors: Vec<PiiDetector>) -> PiiSpec {
        PiiSpec {
            detectors,
            scope: ExportScope::All,
        }
    }

    fn run(d: &Document, s: &PiiSpec) -> PiiReport {
        let registry = JobRegistry::default();
        let ctx = registry.begin("pii", None, |_| {});
        scan(d, s, &ctx).unwrap()
    }

    // A Luhn-valid test number (the classic Visa test card).
    const VALID_CARD: &str = "4111 1111 1111 1111";
    // Same digits with the last one changed: Luhn-invalid.
    const INVALID_CARD: &str = "4111 1111 1111 1112";

    #[test]
    fn luhn_invalid_card_like_numbers_are_never_classified() {
        let d = doc(&format!("card,x\n{VALID_CARD},1\n{INVALID_CARD},1\n"));
        let r = run(&d, &spec(vec![PiiDetector::PaymentCard]));
        assert_eq!(r.total_matches, 1, "only the Luhn-valid number");
        assert_eq!(r.findings[0].validation, "pattern + Luhn checksum");
    }

    #[test]
    fn masked_samples_never_expose_full_values() {
        let d = doc(&format!("card,ssn\n{VALID_CARD},123-45-6789\n"));
        let r = run(&d, &spec(vec![PiiDetector::PaymentCard, PiiDetector::Ssn]));
        for finding in &r.findings {
            for sample in &finding.samples {
                assert!(!sample.contains("4111 1111 1111"), "card masked: {sample}");
                assert!(!sample.contains("123-45"), "ssn masked: {sample}");
                assert!(sample.contains('•'), "visibly masked: {sample}");
            }
        }
        // The card sample keeps only the last 4.
        let card = r.findings.iter().find(|f| f.detector == 0).unwrap();
        assert!(card.samples[0].ends_with("1111"));
        assert!(!card.samples[0].starts_with('4'));
    }

    #[test]
    fn ssn_pattern_excludes_invalid_areas() {
        assert!(is_ssn("123-45-6789"));
        assert!(!is_ssn("000-45-6789"));
        assert!(!is_ssn("666-45-6789"));
        assert!(!is_ssn("900-45-6789"));
        assert!(!is_ssn("123-00-6789"));
        assert!(!is_ssn("123-45-0000"));
        assert!(!is_ssn("123456789"));
    }

    #[test]
    fn email_phone_ip_and_custom_detectors_match() {
        let d = doc("a,b\nuser@example.com,10.0.0.1\n+1 555-123-4567,ACC-12345\nplain,text\n");
        let s = spec(vec![
            PiiDetector::Email,
            PiiDetector::PhoneNumber,
            PiiDetector::IpAddress,
            PiiDetector::Custom {
                name: "account".into(),
                pattern: r"ACC-\d{5}".into(),
            },
        ]);
        let r = run(&d, &s);
        assert_eq!(r.total_matches, 4);

        // Invalid custom patterns are rejected before scanning.
        let bad = spec(vec![PiiDetector::Custom {
            name: "broken".into(),
            pattern: "(".into(),
        }]);
        let registry = JobRegistry::default();
        let ctx = registry.begin("pii", None, |_| {});
        assert!(scan(&d, &bad, &ctx).is_err());
    }

    #[test]
    fn pseudonyms_are_stable_per_key_and_salt_and_differ_across_salts() {
        let a = pseudonym("secret", "salt1", "user@example.com");
        let b = pseudonym("secret", "salt1", "user@example.com");
        let c = pseudonym("secret", "salt2", "user@example.com");
        let d = pseudonym("other", "salt1", "user@example.com");
        assert_eq!(a, b, "same key + salt -> same pseudonym");
        assert_ne!(a, c, "different salt -> different pseudonym");
        assert_ne!(a, d, "different key -> different pseudonym");
        assert!(a.starts_with("pii_"));
        assert!(!a.contains("user"), "pseudonym leaks nothing");
    }

    #[test]
    fn generated_salts_are_unique_and_hex() {
        let a = generate_salt().unwrap();
        let b = generate_salt().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn redactions_apply_as_one_undo_and_previews_are_masked() {
        let mut d = doc(&format!("card,x\n{VALID_CARD},keep\nplain,keep\n"));
        let s = spec(vec![PiiDetector::PaymentCard]);
        let computed =
            redaction_changes(&d, &s, 0, 0, &RedactionAction::KeepLast { n: 4 }).unwrap();
        assert_eq!(computed.preview.cells_affected, 1);
        let (masked_before, after) = &computed.preview.examples[0];
        assert!(!masked_before.contains("4111 1111 1111"));
        assert!(after.ends_with("1111") && after.starts_with('•'));

        d.set_cells(computed.changes).unwrap();
        assert!(d.rows()[0][0].ends_with("1111"));
        assert!(!d.rows()[0][0].contains('4'));
        assert_eq!(d.rows()[1][0], "plain", "non-matching cells untouched");
        d.undo().unwrap();
        assert_eq!(d.rows()[0][0], VALID_CARD);
        assert!(!d.can_undo(), "one undo step");
    }

    #[test]
    fn pseudonymize_and_structural_redactions() {
        let d = doc("email,x\nuser@example.com,1\nother@example.com,2\nplain,3\n");
        let s = spec(vec![PiiDetector::Email]);
        let computed = redaction_changes(
            &d,
            &s,
            0,
            0,
            &RedactionAction::Pseudonymize {
                secret: "s3cret".into(),
                salt: None,
            },
        )
        .unwrap();
        assert_eq!(computed.changes.len(), 2);
        let salt = computed.preview.salt.clone().unwrap();
        assert!(computed.changes[0].2.starts_with("pii_"));
        // Reusing the echoed salt reproduces the same pseudonyms.
        let again = redaction_changes(
            &d,
            &s,
            0,
            0,
            &RedactionAction::Pseudonymize {
                secret: "s3cret".into(),
                salt: Some(salt),
            },
        )
        .unwrap();
        assert_eq!(computed.changes[0].2, again.changes[0].2);

        let rows = redaction_changes(&d, &s, 0, 0, &RedactionAction::RemoveRows).unwrap();
        assert_eq!(rows.remove_rows, vec![0, 1]);
        let col = redaction_changes(&d, &s, 0, 0, &RedactionAction::RemoveColumn).unwrap();
        assert_eq!(col.remove_column, Some(0));
        assert!(col.preview.column_removed);
    }

    #[test]
    fn scan_never_mutates_or_dirties() {
        let d = doc("email,x\nuser@example.com,1\n");
        let before = d.revision();
        let _ = run(&d, &spec(vec![PiiDetector::Email]));
        assert_eq!(d.revision(), before);
        assert_eq!(d.rows()[0][0], "user@example.com");
    }

    #[test]
    fn invalid_configs_are_rejected() {
        let d = doc("a\n1\n");
        let s = spec(vec![PiiDetector::Email]);
        assert!(redaction_changes(&d, &s, 9, 0, &RedactionAction::FullMask).is_err());
        assert!(redaction_changes(&d, &s, 0, 9, &RedactionAction::FullMask).is_err());
        assert!(redaction_changes(&d, &s, 0, 0, &RedactionAction::KeepLast { n: 0 }).is_err());
        assert!(redaction_changes(
            &d,
            &s,
            0,
            0,
            &RedactionAction::Pseudonymize {
                secret: String::new(),
                salt: None,
            },
        )
        .is_err());
        let empty = PiiSpec {
            detectors: vec![],
            scope: ExportScope::All,
        };
        let registry = JobRegistry::default();
        let ctx = registry.begin("pii", None, |_| {});
        assert!(scan(&d, &empty, &ctx).is_err());
    }
}
