//! Explicit logical schemas and typed columns (F31).
//!
//! A schema is a LOGICAL layer over the document: assigning a type never
//! rewrites cell text, and display formatting never touches storage. The only
//! way typed values reach the grid is the explicit (separately implemented)
//! canonical-conversion action, which goes through the ordinary undoable edit
//! paths.
//!
//! Five distinguishable cell states (see [`CellState`]):
//! * `Missing` — the field did not exist in the source record (ragged short
//!   row). The in-memory grid is padded rectangular, so callers pass `None`
//!   for cells the import diagnostics know were absent.
//! * `NullToken` — the (trimmed) cell text exactly matches one of the
//!   schema's configured `nullTokens` ("NULL", "N/A", even "").
//! * `Empty` — the cell is empty/whitespace-only and NOT a configured token.
//! * `Valid(v)` — the text parses as the declared logical type.
//! * `Invalid(reason)` — it does not.
//!
//! Locale-aware numbers use a small built-in separator table (see
//! [`separators`]) instead of ICU: `"de-DE"` reads `1.234,5`, `"fr-FR"` reads
//! `1 234,5` (regular, no-break and narrow no-break spaces), `"de-CH"` reads
//! `1'234.5`; everything else defaults to `1,234.5`. Grouping separators must
//! sit between digits in groups of exactly three, so `"1.5"` under `de-DE` is
//! rejected rather than silently read as 15.
//!
//! Datetimes: values carrying an explicit UTC offset (via `%z`-style input
//! formats or the RFC 3339 fallback) are normalised to UTC. Naive values are
//! interpreted in the schema's `timeZone` (chrono-tz) when one is set — DST
//! folds resolve to the EARLIEST instant, DST gaps are invalid — and stay
//! naive wall time otherwise. [`TypedValue::DateTime`] therefore holds UTC
//! when any zone information was available and wall time when none was.
//!
//! `displayFormat` is a small closed catalogue (unknown patterns are ignored
//! and the raw text shown):
//! * numbers (`integer` / `decimal` / `float`):
//!   - `"thousands"` — locale grouping: `1,234,567.5` / `1.234.567,5`
//!   - `"fixed:N"` — N decimal places (0–12), half-away-from-zero rounding
//!   - `"percent"` — value × 100 with a trailing `%` (exact for
//!     integer/decimal)
//! * dates (`date` / `datetime`):
//!   - `"iso"` — `2024-01-31` / `2024-01-31 15:04:05`
//!   - `"eu"` — `31.01.2024` (+ ` 15:04:05`)
//!   - `"us"` — `01/31/2024` (+ ` 15:04:05`)
//!   - `"long"` — `Jan 31, 2024` (+ ` 15:04`)

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, Datelike, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};

use crate::analyze;
use crate::document::Document;
use crate::error::{AppError, AppResult};

/// Version written to (and required from) schema import/export envelopes.
pub const SCHEMA_VERSION: u32 = 1;

/// Rows scanned by [`infer_schema`] on an INDEXED document (editable
/// documents scan everything). Mirrors the F26 semantic-scan sample.
const INFER_SAMPLE_ROWS: usize = 100_000;

/// Null-ish tokens recognised by inference (exact match on the trimmed cell).
/// Only tokens actually OBSERVED in a column end up in its `nullTokens`.
const NULLISH_TOKENS: &[&str] = &[
    "NULL", "null", "Null", "N/A", "n/a", "NA", "na", "NaN", "nan", "None", "none", "NONE", "nil",
    "NIL", "-", "--", "(null)", "#N/A",
];

// ---------------------------------------------------------------------------
// Schema model (wire DTOs, camelCase)
// ---------------------------------------------------------------------------

/// The nine declarable logical types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogicalType {
    Text,
    Integer,
    Decimal,
    Float,
    Boolean,
    Date,
    Datetime,
    Uuid,
    Json,
}

/// How edit validation behaves: `advisory` records issues without blocking,
/// `strict` rejects invalid edits before they reach the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValidationMode {
    #[default]
    Advisory,
    Strict,
}

/// One column's declared schema. Keyed by the STABLE column ID (F12), never
/// by position or header text, so assignments survive renames and reorders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSchema {
    pub column_id: String,
    /// Display name (the header at assignment time; refresh from the
    /// document by ID, the header is the source of truth).
    pub name: String,
    pub logical_type: LogicalType,
    #[serde(default = "default_true")]
    pub nullable: bool,
    /// Cell texts (compared trimmed, case-sensitively) that mean "no value".
    #[serde(default)]
    pub null_tokens: Vec<String>,
    /// BCP-47-ish tag ("de-DE") selecting number separators; `None` = `1,234.5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// IANA zone ("Europe/Berlin") naive datetimes are interpreted in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    /// chrono strftime patterns tried in order for date/datetime parsing.
    /// `None`/empty = the built-in format lists plus RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_formats: Option<Vec<String>>,
    /// Display-only pattern from the documented catalogue (module docs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_format: Option<String>,
    #[serde(default)]
    pub validation_mode: ValidationMode,
}

fn default_true() -> bool {
    true
}

impl ColumnSchema {
    /// A permissive default schema: nullable, no tokens, advisory.
    pub fn new(column_id: impl Into<String>, name: impl Into<String>, lt: LogicalType) -> Self {
        ColumnSchema {
            column_id: column_id.into(),
            name: name.into(),
            logical_type: lt,
            nullable: true,
            null_tokens: Vec::new(),
            locale: None,
            time_zone: None,
            input_formats: None,
            display_format: None,
            validation_mode: ValidationMode::Advisory,
        }
    }
}

/// A document's schema: per-column entries keyed by stable column ID.
/// Columns without an entry are implicitly plain text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSchema {
    pub columns: BTreeMap<String, ColumnSchema>,
}

impl DocumentSchema {
    pub fn column(&self, column_id: &str) -> Option<&ColumnSchema> {
        self.columns.get(column_id)
    }

    pub fn set_column(&mut self, schema: ColumnSchema) {
        self.columns.insert(schema.column_id.clone(), schema);
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

/// Versioned import/export envelope: `{ "version": 1, "columns": [...] }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaExport {
    pub version: u32,
    pub columns: Vec<ColumnSchema>,
}

/// Build an export envelope, emitting columns in `column_order` (the
/// document's current ID order) first, then any remaining entries.
pub fn export_schema(schema: &DocumentSchema, column_order: &[String]) -> SchemaExport {
    let mut columns = Vec::with_capacity(schema.columns.len());
    let mut seen: HashSet<&str> = HashSet::new();
    for id in column_order {
        if let Some(col) = schema.columns.get(id) {
            if seen.insert(id.as_str()) {
                columns.push(col.clone());
            }
        }
    }
    for (id, col) in &schema.columns {
        if !seen.contains(id.as_str()) {
            columns.push(col.clone());
        }
    }
    SchemaExport {
        version: SCHEMA_VERSION,
        columns,
    }
}

/// Serialize a schema to pretty JSON for export.
pub fn export_to_json(schema: &DocumentSchema, column_order: &[String]) -> AppResult<String> {
    serde_json::to_string_pretty(&export_schema(schema, column_order))
        .map_err(|e| AppError::invalid(format!("could not serialize schema: {e}")))
}

/// Validate and unpack an import envelope. Unknown versions and duplicate
/// column IDs are rejected; unknown JSON fields are ignored (forward-tolerant
/// within a version).
pub fn import_schema(export: SchemaExport) -> AppResult<DocumentSchema> {
    if export.version != SCHEMA_VERSION {
        return Err(AppError::invalid(format!(
            "unsupported schema version {} (this build reads version {SCHEMA_VERSION})",
            export.version
        )));
    }
    let mut columns = BTreeMap::new();
    for col in export.columns {
        let id = col.column_id.clone();
        if columns.insert(id.clone(), col).is_some() {
            return Err(AppError::invalid(format!(
                "duplicate columnId \"{id}\" in schema"
            )));
        }
    }
    Ok(DocumentSchema { columns })
}

/// Parse exported-schema JSON. The version field is probed FIRST so an
/// incompatible future format fails with the version message, not a shape
/// error.
pub fn import_from_json(json: &str) -> AppResult<DocumentSchema> {
    #[derive(Deserialize)]
    struct VersionProbe {
        version: u32,
    }
    let probe: VersionProbe = serde_json::from_str(json)
        .map_err(|e| AppError::invalid(format!("invalid schema JSON: {e}")))?;
    if probe.version != SCHEMA_VERSION {
        return Err(AppError::invalid(format!(
            "unsupported schema version {} (this build reads version {SCHEMA_VERSION})",
            probe.version
        )));
    }
    let export: SchemaExport = serde_json::from_str(json)
        .map_err(|e| AppError::invalid(format!("invalid schema JSON: {e}")))?;
    import_schema(export)
}

// ---------------------------------------------------------------------------
// Typed values and classification
// ---------------------------------------------------------------------------

/// An exact decimal: `(-1 if negative) × digits × 10^-scale`. Digits are an
/// ASCII string with no leading zeros ("0" alone for zero) so precision is
/// preserved exactly — `1.50` keeps scale 2 and round-trips as `1.50`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecimalValue {
    pub negative: bool,
    pub digits: String,
    pub scale: u32,
}

impl DecimalValue {
    /// Integer and fraction digit strings (`"1.50"` → `("1", "50")`).
    fn parts(&self) -> (String, String) {
        let s = self.scale as usize;
        let d = &self.digits;
        if s == 0 {
            (d.clone(), String::new())
        } else if d.len() > s {
            (d[..d.len() - s].to_string(), d[d.len() - s..].to_string())
        } else {
            ("0".to_string(), format!("{}{}", "0".repeat(s - d.len()), d))
        }
    }

    /// Canonical plain notation: ASCII digits, `.` separator, `-` sign.
    pub fn to_plain_string(&self) -> String {
        let (int_part, frac_part) = self.parts();
        let mut out = String::new();
        if self.negative {
            out.push('-');
        }
        out.push_str(&int_part);
        if !frac_part.is_empty() {
            out.push('.');
            out.push_str(&frac_part);
        }
        out
    }

    /// Exact rescale to `target` fraction digits, rounding half-away-from-zero
    /// when digits are dropped (string arithmetic, no binary float error).
    fn rescaled(&self, target: u32) -> DecimalValue {
        if target >= self.scale {
            let mut digits = self.digits.clone();
            if digits != "0" {
                digits.push_str(&"0".repeat((target - self.scale) as usize));
            }
            return DecimalValue {
                negative: self.negative,
                digits,
                scale: target,
            };
        }
        let drop = (self.scale - target) as usize;
        let d = &self.digits;
        let (kept, first_dropped) = if d.len() > drop {
            (
                d[..d.len() - drop].to_string(),
                d.as_bytes()[d.len() - drop],
            )
        } else if d.len() == drop {
            ("0".to_string(), d.as_bytes()[0])
        } else {
            // All significant digits are dropped AND below the first dropped
            // position, e.g. 0.005 at one decimal: the leading dropped digit
            // is a padding zero, so the value rounds to zero.
            ("0".to_string(), b'0')
        };
        let rounded = if first_dropped >= b'5' {
            increment_digits(&kept)
        } else {
            kept
        };
        let trimmed = rounded.trim_start_matches('0');
        let kept = if trimmed.is_empty() {
            "0".to_string()
        } else {
            trimmed.to_string()
        };
        DecimalValue {
            negative: self.negative && kept != "0",
            digits: kept,
            scale: target,
        }
    }

    /// Exact × 100 (for the `"percent"` display pattern).
    fn times_100(&self) -> DecimalValue {
        if self.scale >= 2 {
            return DecimalValue {
                negative: self.negative,
                digits: self.digits.clone(),
                scale: self.scale - 2,
            };
        }
        let mut digits = self.digits.clone();
        if digits != "0" {
            digits.push_str(&"0".repeat((2 - self.scale) as usize));
        }
        DecimalValue {
            negative: self.negative,
            digits,
            scale: 0,
        }
    }
}

/// Add one to an ASCII digit string ("999" → "1000").
fn increment_digits(d: &str) -> String {
    let mut bytes: Vec<u8> = d.bytes().collect();
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        if bytes[i] == b'9' {
            bytes[i] = b'0';
        } else {
            bytes[i] += 1;
            return String::from_utf8(bytes).expect("ascii digits");
        }
    }
    let mut out = String::with_capacity(bytes.len() + 1);
    out.push('1');
    out.push_str(std::str::from_utf8(&bytes).expect("ascii digits"));
    out
}

impl LogicalType {
    /// The numeric types (locale-aware parsing, numeric ordering).
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            LogicalType::Integer | LogicalType::Decimal | LogicalType::Float
        )
    }

    /// The temporal types (chronological ordering).
    pub fn is_temporal(self) -> bool {
        matches!(self, LogicalType::Date | LogicalType::Datetime)
    }

    /// Whether declared values have a meaningful non-lexicographic order
    /// (numeric, chronological or boolean) that sorting and range filters
    /// should prefer over the text heuristics.
    pub fn has_typed_order(self) -> bool {
        self.is_numeric() || self.is_temporal() || self == LogicalType::Boolean
    }
}

/// The coarse [`crate::dto::ColumnKind`] a declared logical type maps to, for
/// the surfaces (grid badges, profiles) that predate the nine-type schema.
pub fn column_kind_of(lt: LogicalType) -> crate::dto::ColumnKind {
    use crate::dto::ColumnKind;
    match lt {
        LogicalType::Integer | LogicalType::Decimal | LogicalType::Float => ColumnKind::Number,
        LogicalType::Boolean => ColumnKind::Bool,
        LogicalType::Date | LogicalType::Datetime => ColumnKind::Date,
        LogicalType::Text | LogicalType::Uuid | LogicalType::Json => ColumnKind::Text,
    }
}

/// Whether the (trimmed) cell matches one of the schema's null tokens.
pub fn is_null_token(schema: &ColumnSchema, cell: &str) -> bool {
    let trimmed = cell.trim();
    schema.null_tokens.iter().any(|t| t.as_str() == trimmed)
}

/// A successfully parsed cell value.
#[derive(Debug, Clone, PartialEq)]
pub enum TypedValue {
    Text(String),
    /// i128 so the full i64 range (and beyond) round-trips exactly.
    Integer(i128),
    Decimal(DecimalValue),
    Float(f64),
    Boolean(bool),
    Date(NaiveDate),
    /// UTC when any zone information was available, naive wall time otherwise
    /// (module docs).
    DateTime(NaiveDateTime),
    /// Canonical lowercase hyphenated form.
    Uuid(String),
    Json(serde_json::Value),
}

/// The five-way classification of one cell against a column schema.
#[derive(Debug, Clone, PartialEq)]
pub enum CellState {
    /// Field absent from the source record (caller passed `None`).
    Missing,
    /// Matches a configured null token.
    NullToken,
    /// Empty/whitespace-only, and not a configured token.
    Empty,
    Valid(TypedValue),
    Invalid(String),
}

/// Classify a raw cell against a schema. Pure state: `nullable` is applied by
/// [`validate_value`], not here, so callers can distinguish "empty" from
/// "empty AND disallowed".
pub fn classify(raw: Option<&str>, schema: &ColumnSchema) -> CellState {
    let Some(raw) = raw else {
        return CellState::Missing;
    };
    let trimmed = raw.trim();
    if schema.null_tokens.iter().any(|t| t.as_str() == trimmed) {
        return CellState::NullToken;
    }
    if trimmed.is_empty() {
        return CellState::Empty;
    }
    match parse_typed(trimmed, schema) {
        Ok(value) => CellState::Valid(value),
        Err(reason) => CellState::Invalid(reason),
    }
}

/// Validate a proposed edit value against a schema: `Ok(())` or the reason it
/// is unacceptable. The strict path rejects on `Err`; the advisory path
/// applies the edit and records the reason as a validation issue.
pub fn validate_value(schema: &ColumnSchema, proposed: &str) -> Result<(), String> {
    match classify(Some(proposed), schema) {
        CellState::Valid(_) => Ok(()),
        CellState::NullToken => {
            if schema.nullable {
                Ok(())
            } else {
                Err(format!(
                    "column \"{}\" is not nullable — null tokens are not allowed",
                    schema.name
                ))
            }
        }
        CellState::Empty | CellState::Missing => {
            // An empty string IS a valid (empty) text value; for every other
            // type an empty cell is a null and needs nullable.
            if schema.nullable || schema.logical_type == LogicalType::Text {
                Ok(())
            } else {
                Err(format!(
                    "column \"{}\" is not nullable — empty values are not allowed",
                    schema.name
                ))
            }
        }
        CellState::Invalid(reason) => Err(reason),
    }
}

/// The canonical text for a cell, when it is valid: `Some(canonical)` for
/// `Valid` cells (identical to `raw` for text), `None` for every other state.
/// The explicit conversion action uses this; nothing else ever rewrites text.
pub fn canonical_text(schema: &ColumnSchema, raw: &str) -> Option<String> {
    match classify(Some(raw), schema) {
        CellState::Valid(value) => Some(match value {
            TypedValue::Text(_) => raw.to_string(),
            TypedValue::Integer(v) => v.to_string(),
            TypedValue::Decimal(d) => d.to_plain_string(),
            TypedValue::Float(f) => f.to_string(),
            TypedValue::Boolean(b) => b.to_string(),
            TypedValue::Date(d) => d.format("%Y-%m-%d").to_string(),
            TypedValue::DateTime(dt) => dt.format("%Y-%m-%dT%H:%M:%S").to_string(),
            TypedValue::Uuid(u) => u,
            TypedValue::Json(j) => serde_json::to_string(&j).unwrap_or_else(|_| raw.to_string()),
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Typed parsing
// ---------------------------------------------------------------------------

/// Parse a trimmed, non-empty cell as the schema's logical type.
pub fn parse_typed(trimmed: &str, schema: &ColumnSchema) -> Result<TypedValue, String> {
    let sep = separators(schema.locale.as_deref());
    match schema.logical_type {
        LogicalType::Text => Ok(TypedValue::Text(trimmed.to_string())),
        LogicalType::Integer => {
            let normalized = normalize_number(trimmed, &sep, false, false)?;
            normalized
                .parse::<i128>()
                .map(TypedValue::Integer)
                .map_err(|_| "integer out of range".to_string())
        }
        LogicalType::Decimal => {
            let normalized = normalize_number(trimmed, &sep, true, false)?;
            decimal_from_normalized(&normalized).map(TypedValue::Decimal)
        }
        LogicalType::Float => {
            let normalized = normalize_number(trimmed, &sep, true, true)?;
            let value: f64 = normalized
                .parse()
                .map_err(|_| "not a valid number".to_string())?;
            if value.is_finite() {
                Ok(TypedValue::Float(value))
            } else {
                Err("not a finite number".to_string())
            }
        }
        LogicalType::Boolean => match trimmed.to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Ok(TypedValue::Boolean(true)),
            "false" | "no" | "0" => Ok(TypedValue::Boolean(false)),
            _ => Err("not a boolean (expected true/false, yes/no or 1/0)".to_string()),
        },
        LogicalType::Date => parse_date_value(trimmed, schema).map(TypedValue::Date),
        LogicalType::Datetime => parse_datetime_value(trimmed, schema).map(TypedValue::DateTime),
        LogicalType::Uuid => {
            if crate::semantic::matches_type(trimmed, crate::semantic::SemanticType::Uuid) {
                Ok(TypedValue::Uuid(trimmed.to_ascii_lowercase()))
            } else {
                Err("not a valid UUID (expected 8-4-4-4-12 hex digits)".to_string())
            }
        }
        LogicalType::Json => serde_json::from_str::<serde_json::Value>(trimmed)
            .map(TypedValue::Json)
            .map_err(|e| format!("not valid JSON: {e}")),
    }
}

/// Number separators for a locale tag (module docs for the table).
struct Separators {
    decimal: char,
    group_out: char,
    group_accept: &'static [char],
}

fn separators(locale: Option<&str>) -> Separators {
    let tag = locale.unwrap_or("");
    // Full-tag overrides first: Swiss number formatting groups with an
    // apostrophe regardless of language.
    if matches!(tag, "de-CH" | "fr-CH" | "it-CH" | "en-CH") {
        return Separators {
            decimal: '.',
            group_out: '\'',
            group_accept: &['\'', '\u{2019}'],
        };
    }
    let lang = tag.split(['-', '_']).next().unwrap_or("");
    match lang {
        // Comma decimal, dot grouping.
        "de" | "es" | "it" | "nl" | "pt" | "da" | "el" | "id" | "tr" | "vi" | "hr" | "sl"
        | "ro" | "ca" => Separators {
            decimal: ',',
            group_out: '.',
            group_accept: &['.'],
        },
        // Comma decimal, space grouping (regular, NBSP and narrow NBSP all
        // accepted on input; NBSP emitted on output).
        "fr" | "ru" | "pl" | "cs" | "sk" | "sv" | "fi" | "nb" | "nn" | "no" | "uk" | "lt"
        | "lv" | "et" | "hu" | "bg" => Separators {
            decimal: ',',
            group_out: '\u{00A0}',
            group_accept: &[' ', '\u{00A0}', '\u{202F}'],
        },
        // Default (en, ja, zh, ko, …): dot decimal, comma grouping.
        _ => Separators {
            decimal: '.',
            group_out: ',',
            group_accept: &[','],
        },
    }
}

/// Normalise a locale-formatted number to plain ASCII (`.` decimal, no
/// grouping). Grouping separators must be flanked by digits and followed by a
/// group of exactly three; the decimal separator may appear once; exponents
/// (`e`/`E`, float only) end grouping and decimal territory.
fn normalize_number(
    s: &str,
    sep: &Separators,
    allow_decimal: bool,
    allow_exponent: bool,
) -> Result<String, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut seen_decimal = false;
    let mut seen_exponent = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '+' || c == '-' {
            let after_exponent = out.ends_with('e');
            if !(out.is_empty() || after_exponent) {
                return Err("misplaced sign".to_string());
            }
            out.push(c);
        } else if c.is_ascii_digit() {
            out.push(c);
        } else if !seen_exponent && !seen_decimal && sep.group_accept.contains(&c) {
            let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            let run = j - (i + 1);
            let boundary_ok = j == chars.len()
                || chars[j] == sep.decimal
                || sep.group_accept.contains(&chars[j])
                || (allow_exponent && (chars[j] == 'e' || chars[j] == 'E'));
            if !(prev_digit && run == 3 && boundary_ok) {
                return Err("misplaced grouping separator".to_string());
            }
            // Valid group separator: skip it.
        } else if c == sep.decimal && allow_decimal && !seen_exponent {
            if seen_decimal {
                return Err("multiple decimal separators".to_string());
            }
            seen_decimal = true;
            out.push('.');
        } else if (c == 'e' || c == 'E') && allow_exponent {
            if seen_exponent {
                return Err("multiple exponents".to_string());
            }
            seen_exponent = true;
            out.push('e');
        } else {
            return Err(format!("unexpected character '{c}'"));
        }
        i += 1;
    }
    if !out.chars().any(|c| c.is_ascii_digit()) {
        return Err("no digits".to_string());
    }
    Ok(out)
}

/// Build a [`DecimalValue`] from normalised text (`[+-]?digits[.digits]`).
fn decimal_from_normalized(n: &str) -> Result<DecimalValue, String> {
    let (neg, rest) = if let Some(r) = n.strip_prefix('-') {
        (true, r)
    } else {
        (false, n.strip_prefix('+').unwrap_or(n))
    };
    let (int_part, frac_part) = match rest.split_once('.') {
        Some((i, f)) => (i, f),
        None => (rest, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return Err("not a valid number".to_string());
    }
    let combined = format!("{int_part}{frac_part}");
    let trimmed = combined.trim_start_matches('0');
    let digits = if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    };
    let negative = neg && digits != "0";
    Ok(DecimalValue {
        negative,
        digits,
        scale: frac_part.len() as u32,
    })
}

/// Detection-guard year range for the DEFAULT formats (custom `inputFormats`
/// are trusted as-is: `%y` legitimately yields 20xx).
fn year_in_range(y: i32) -> bool {
    (1000..=9999).contains(&y)
}

/// Custom formats as `&str`s, when configured and non-empty.
fn custom_formats(schema: &ColumnSchema) -> Option<Vec<&str>> {
    schema
        .input_formats
        .as_ref()
        .filter(|v| !v.is_empty())
        .map(|v| v.iter().map(String::as_str).collect())
}

fn parse_date_value(s: &str, schema: &ColumnSchema) -> Result<NaiveDate, String> {
    let custom = custom_formats(schema);
    let (formats, is_custom): (&[&str], bool) = match &custom {
        Some(v) => (v.as_slice(), true),
        None => (analyze::DATE_FORMATS, false),
    };
    for fmt in formats {
        if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
            if is_custom || year_in_range(d.year()) {
                return Ok(d);
            }
        }
    }
    Err(if is_custom {
        "does not match any configured input format".to_string()
    } else {
        "not a recognised date".to_string()
    })
}

fn parse_datetime_value(s: &str, schema: &ColumnSchema) -> Result<NaiveDateTime, String> {
    let custom = custom_formats(schema);
    let defaults: Vec<&str> = analyze::DATETIME_FORMATS
        .iter()
        .chain(analyze::DATE_FORMATS.iter())
        .copied()
        .collect();
    let (formats, is_custom): (&[&str], bool) = match &custom {
        Some(v) => (v.as_slice(), true),
        None => (defaults.as_slice(), false),
    };
    for fmt in formats {
        // Offset-aware first (formats carrying %z): explicit offsets win over
        // the schema's timeZone and normalise straight to UTC.
        if let Ok(dt) = DateTime::parse_from_str(s, fmt) {
            if is_custom || year_in_range(dt.year()) {
                return Ok(dt.naive_utc());
            }
        }
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
            if is_custom || year_in_range(ndt.year()) {
                return resolve_zone(ndt, schema);
            }
        }
        // Date-only inputs land at midnight (in the schema zone, if any).
        if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
            if is_custom || year_in_range(d.year()) {
                let midnight = d.and_hms_opt(0, 0, 0).expect("midnight is valid");
                return resolve_zone(midnight, schema);
            }
        }
    }
    if !is_custom {
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return Ok(dt.naive_utc());
        }
    }
    Err(if is_custom {
        "does not match any configured input format".to_string()
    } else {
        "not a recognised date-time".to_string()
    })
}

/// Interpret a naive wall time in the schema's zone (module docs: folds →
/// earliest, gaps → invalid) and normalise to UTC; identity without a zone.
fn resolve_zone(ndt: NaiveDateTime, schema: &ColumnSchema) -> Result<NaiveDateTime, String> {
    let Some(name) = schema.time_zone.as_deref() else {
        return Ok(ndt);
    };
    let tz: chrono_tz::Tz = name
        .parse()
        .map_err(|_| format!("unknown time zone \"{name}\""))?;
    match tz.from_local_datetime(&ndt) {
        LocalResult::Single(dt) => Ok(dt.naive_utc()),
        LocalResult::Ambiguous(earliest, _) => Ok(earliest.naive_utc()),
        LocalResult::None => Err(format!("nonexistent local time in {name} (DST gap)")),
    }
}

// ---------------------------------------------------------------------------
// Display formatting (never touches storage)
// ---------------------------------------------------------------------------

enum NumberPattern {
    Thousands,
    Fixed(u32),
    Percent,
}

fn number_pattern(fmt: &str) -> Option<NumberPattern> {
    match fmt {
        "thousands" => Some(NumberPattern::Thousands),
        "percent" => Some(NumberPattern::Percent),
        _ => fmt
            .strip_prefix("fixed:")
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|&n| n <= 12)
            .map(NumberPattern::Fixed),
    }
}

fn date_pattern(fmt: &str, with_time: bool) -> Option<&'static str> {
    match (fmt, with_time) {
        ("iso", false) => Some("%Y-%m-%d"),
        ("iso", true) => Some("%Y-%m-%d %H:%M:%S"),
        ("eu", false) => Some("%d.%m.%Y"),
        ("eu", true) => Some("%d.%m.%Y %H:%M:%S"),
        ("us", false) => Some("%m/%d/%Y"),
        ("us", true) => Some("%m/%d/%Y %H:%M:%S"),
        ("long", false) => Some("%b %-d, %Y"),
        ("long", true) => Some("%b %-d, %Y %H:%M"),
        _ => None,
    }
}

/// Group ASCII digits in threes from the right.
fn group_digits(digits: &str, group: char) -> String {
    let n = digits.len();
    let mut out = String::with_capacity(n + n / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(group);
        }
        out.push(c);
    }
    out
}

/// Assemble sign + integer digits (+ fraction) with locale separators.
fn assemble_number(
    negative: bool,
    int_digits: &str,
    frac_digits: &str,
    sep: &Separators,
    group: bool,
) -> String {
    let int_part = if group {
        group_digits(int_digits, sep.group_out)
    } else {
        int_digits.to_string()
    };
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(&int_part);
    if !frac_digits.is_empty() {
        out.push(sep.decimal);
        out.push_str(frac_digits);
    }
    out
}

fn format_integer(v: i128, pattern: &NumberPattern, sep: &Separators) -> Option<String> {
    match pattern {
        NumberPattern::Thousands => {
            let digits = v.unsigned_abs().to_string();
            Some(assemble_number(v < 0, &digits, "", sep, true))
        }
        NumberPattern::Fixed(n) => {
            let digits = v.unsigned_abs().to_string();
            let frac = "0".repeat(*n as usize);
            Some(assemble_number(v < 0, &digits, &frac, sep, false))
        }
        NumberPattern::Percent => {
            let scaled = v.checked_mul(100)?;
            Some(format!("{scaled}%"))
        }
    }
}

fn format_decimal(d: &DecimalValue, pattern: &NumberPattern, sep: &Separators) -> Option<String> {
    match pattern {
        NumberPattern::Thousands => {
            let (int_part, frac_part) = d.parts();
            Some(assemble_number(
                d.negative, &int_part, &frac_part, sep, true,
            ))
        }
        NumberPattern::Fixed(n) => {
            let r = d.rescaled(*n);
            let (int_part, frac_part) = r.parts();
            Some(assemble_number(
                r.negative, &int_part, &frac_part, sep, false,
            ))
        }
        NumberPattern::Percent => {
            let p = d.times_100();
            let (int_part, frac_part) = p.parts();
            let mut out = assemble_number(p.negative, &int_part, &frac_part, sep, false);
            out.push('%');
            Some(out)
        }
    }
}

/// `(negative, int digits, frac digits)` of a float's shortest decimal
/// rendering; `None` when it renders in exponent notation.
fn float_parts(v: f64) -> Option<(bool, String, String)> {
    let s = v.to_string();
    if s.contains(['e', 'E']) {
        return None;
    }
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r.to_string()),
        None => (false, s),
    };
    match rest.split_once('.') {
        Some((i, f)) => Some((neg, i.to_string(), f.to_string())),
        None => Some((neg, rest, String::new())),
    }
}

fn format_float(v: f64, pattern: &NumberPattern, sep: &Separators) -> Option<String> {
    match pattern {
        NumberPattern::Thousands => {
            let (neg, int_part, frac_part) = float_parts(v)?;
            Some(assemble_number(neg, &int_part, &frac_part, sep, true))
        }
        NumberPattern::Fixed(n) => {
            let s = format!("{v:.prec$}", prec = *n as usize);
            let (neg, rest) = match s.strip_prefix('-') {
                Some(r) => (true, r.to_string()),
                None => (false, s),
            };
            let (int_part, frac_part) = match rest.split_once('.') {
                Some((i, f)) => (i.to_string(), f.to_string()),
                None => (rest, String::new()),
            };
            Some(assemble_number(neg, &int_part, &frac_part, sep, false))
        }
        NumberPattern::Percent => {
            let scaled = v * 100.0;
            let (neg, int_part, frac_part) = float_parts(scaled)?;
            let mut out = assemble_number(neg, &int_part, &frac_part, sep, false);
            out.push('%');
            Some(out)
        }
    }
}

/// Render a cell for DISPLAY under the schema's `displayFormat`. Storage is
/// never touched: invalid/empty/null cells, columns without a pattern, and
/// unknown patterns all return the raw text unchanged.
pub fn format_value(schema: &ColumnSchema, raw: &str) -> String {
    let Some(fmt) = schema.display_format.as_deref() else {
        return raw.to_string();
    };
    let CellState::Valid(value) = classify(Some(raw), schema) else {
        return raw.to_string();
    };
    let sep = separators(schema.locale.as_deref());
    let formatted = match value {
        TypedValue::Integer(v) => number_pattern(fmt).and_then(|p| format_integer(v, &p, &sep)),
        TypedValue::Decimal(d) => number_pattern(fmt).and_then(|p| format_decimal(&d, &p, &sep)),
        TypedValue::Float(f) => number_pattern(fmt).and_then(|p| format_float(f, &p, &sep)),
        TypedValue::Date(d) => date_pattern(fmt, false).map(|p| d.format(p).to_string()),
        TypedValue::DateTime(dt) => date_pattern(fmt, true).map(|p| dt.format(p).to_string()),
        // Text, boolean, uuid and json have no display patterns (yet).
        _ => None,
    };
    formatted.unwrap_or_else(|| raw.to_string())
}

// ---------------------------------------------------------------------------
// Typed ordering (sort / filter / group-by prefer the DECLARED type)
// ---------------------------------------------------------------------------

impl DecimalValue {
    /// Numeric sign: -1, 0 or 1.
    fn signum(&self) -> i8 {
        if self.digits == "0" {
            0
        } else if self.negative {
            -1
        } else {
            1
        }
    }

    /// Exact numeric comparison (string arithmetic — no float error, any
    /// precision).
    pub fn cmp_value(&self, other: &DecimalValue) -> Ordering {
        let (sa, sb) = (self.signum(), other.signum());
        if sa != sb {
            return sa.cmp(&sb);
        }
        if sa == 0 {
            return Ordering::Equal;
        }
        // Rescaling UP is exact (zero padding only), so compare at the wider
        // scale. Digits carry no leading zeros: longer means larger.
        let scale = self.scale.max(other.scale);
        let (a, b) = (self.rescaled(scale), other.rescaled(scale));
        let magnitude = a
            .digits
            .len()
            .cmp(&b.digits.len())
            .then_with(|| a.digits.cmp(&b.digits));
        if sa < 0 {
            magnitude.reverse()
        } else {
            magnitude
        }
    }
}

/// Order two typed values of the SAME logical type (one column, one schema).
/// Mismatched variants (impossible for cells classified under one schema)
/// fall back to `Equal`.
pub fn compare_typed(a: &TypedValue, b: &TypedValue) -> Ordering {
    match (a, b) {
        (TypedValue::Text(x), TypedValue::Text(y)) => x.cmp(y),
        (TypedValue::Integer(x), TypedValue::Integer(y)) => x.cmp(y),
        (TypedValue::Decimal(x), TypedValue::Decimal(y)) => x.cmp_value(y),
        // Floats are finite by construction, so partial_cmp is total here.
        (TypedValue::Float(x), TypedValue::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (TypedValue::Boolean(x), TypedValue::Boolean(y)) => x.cmp(y),
        (TypedValue::Date(x), TypedValue::Date(y)) => x.cmp(y),
        (TypedValue::DateTime(x), TypedValue::DateTime(y)) => x.cmp(y),
        (TypedValue::Uuid(x), TypedValue::Uuid(y)) => x.cmp(y),
        (TypedValue::Json(x), TypedValue::Json(y)) => {
            // serde_json::Value has no order; canonical text is deterministic.
            let xs = serde_json::to_string(x).unwrap_or_default();
            let ys = serde_json::to_string(y).unwrap_or_default();
            xs.cmp(&ys)
        }
        _ => Ordering::Equal,
    }
}

/// Rank of a cell state in a typed ordering. Null-ish states sort FIRST
/// ascending — mirroring how empty strings sort before text in the heuristic
/// order — and invalid values sort LAST, grouped after every valid value.
fn state_rank(state: &CellState) -> u8 {
    match state {
        CellState::Missing | CellState::NullToken | CellState::Empty => 0,
        CellState::Valid(_) => 1,
        CellState::Invalid(_) => 2,
    }
}

/// Compare two raw cells under a declared schema: null-ish first, then valid
/// values in TYPED order, then invalid values; raw text breaks ties within
/// the null-ish and invalid bands.
pub fn compare_cells(schema: &ColumnSchema, a: &str, b: &str) -> Ordering {
    let (ca, cb) = (classify(Some(a), schema), classify(Some(b), schema));
    let rank = state_rank(&ca).cmp(&state_rank(&cb));
    if rank != Ordering::Equal {
        return rank;
    }
    match (ca, cb) {
        (CellState::Valid(va), CellState::Valid(vb)) => {
            compare_typed(&va, &vb).then_with(|| a.cmp(b))
        }
        _ => a.cmp(b),
    }
}

/// A declared cell's numeric reading, for aggregation paths that work in f64.
pub enum NumericCell {
    Value(f64),
    /// Empty or a configured null token: skipped, never "invalid".
    Null,
    /// Present but not valid for the declared numeric type.
    Invalid,
}

/// Read a cell of a NUMERIC-typed column as f64 (callers gate on
/// [`LogicalType::is_numeric`]). Integers/decimals beyond f64 precision are
/// approximated — aggregate math is f64 throughout the app.
pub fn numeric_cell(schema: &ColumnSchema, cell: &str) -> NumericCell {
    match classify(Some(cell), schema) {
        CellState::Empty | CellState::NullToken | CellState::Missing => NumericCell::Null,
        CellState::Valid(TypedValue::Integer(i)) => NumericCell::Value(i as f64),
        CellState::Valid(TypedValue::Decimal(d)) => d
            .to_plain_string()
            .parse::<f64>()
            .map(NumericCell::Value)
            .unwrap_or(NumericCell::Invalid),
        CellState::Valid(TypedValue::Float(f)) => NumericCell::Value(f),
        _ => NumericCell::Invalid,
    }
}

/// Read a cell of a TEMPORAL-typed column as a `NaiveDateTime` (dates land at
/// midnight, matching [`crate::analyze::parse_date`]). `None` = null-ish or
/// invalid.
pub fn temporal_cell(schema: &ColumnSchema, cell: &str) -> Option<NaiveDateTime> {
    match classify(Some(cell), schema) {
        CellState::Valid(TypedValue::Date(d)) => d.and_hms_opt(0, 0, 0),
        CellState::Valid(TypedValue::DateTime(dt)) => Some(dt),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Advisory validation issues + schema input validation
// ---------------------------------------------------------------------------

/// Cap on the recorded advisory-validation issues per document.
pub const MAX_SCHEMA_ISSUES: usize = 1000;

/// Longest cell text kept verbatim on a recorded issue.
const ISSUE_VALUE_CAP: usize = 200;

/// One advisory-mode validation issue: an edit that was ACCEPTED although it
/// failed its column's declared type. Recorded outside the undo stack.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaIssue {
    /// Absolute (unfiltered) row index at the time of the edit.
    pub row: usize,
    pub col: usize,
    pub column_id: String,
    /// The offending value (truncated to a bounded length).
    pub value: String,
    pub reason: String,
    /// Document revision AFTER the edit was applied.
    pub revision: u64,
}

impl SchemaIssue {
    pub fn new(
        row: usize,
        col: usize,
        column_id: impl Into<String>,
        value: &str,
        reason: impl Into<String>,
        revision: u64,
    ) -> SchemaIssue {
        let truncated = value.chars().nth(ISSUE_VALUE_CAP).is_some();
        let mut value: String = value.chars().take(ISSUE_VALUE_CAP).collect();
        if truncated {
            value.push('…');
        }
        SchemaIssue {
            row,
            col,
            column_id: column_id.into(),
            value,
            reason: reason.into(),
            revision,
        }
    }
}

/// Validate a column schema COMING FROM the front end before storing it.
/// Unknown display formats are legal (ignored at render time); a time zone
/// or input-format list that can never parse anything is not.
pub fn validate_column_schema(schema: &ColumnSchema) -> AppResult<()> {
    if schema.column_id.trim().is_empty() {
        return Err(AppError::invalid("columnId must not be empty"));
    }
    if let Some(tz) = schema.time_zone.as_deref() {
        if tz.parse::<chrono_tz::Tz>().is_err() {
            return Err(AppError::invalid(format!("unknown time zone \"{tz}\"")));
        }
    }
    if let Some(formats) = schema.input_formats.as_ref() {
        if formats.iter().any(|f| f.trim().is_empty()) {
            return Err(AppError::invalid("input formats must not be empty strings"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Inference
// ---------------------------------------------------------------------------

/// Candidate types in PRIORITY order: the first candidate every non-null cell
/// of a column satisfies wins. Integer precedes boolean so 0/1 flag columns
/// stay numeric (mirroring [`crate::analyze::is_bool`]'s exclusion of 0/1);
/// decimal precedes float so plain-notation numbers keep exact precision;
/// date precedes datetime so pure-date columns are dates; json is last
/// because bare numbers and booleans are also valid JSON.
const INFER_CANDIDATES: [LogicalType; 8] = [
    LogicalType::Integer,
    LogicalType::Decimal,
    LogicalType::Float,
    LogicalType::Boolean,
    LogicalType::Date,
    LogicalType::Datetime,
    LogicalType::Uuid,
    LogicalType::Json,
];

/// Numeric text with a superfluous leading zero ("00501"): parsing it as a
/// number would drop the zeros, so inference refuses integer/decimal for it
/// (ZIP-code protection) and the column stays text unless declared otherwise.
fn has_leading_zero(s: &str) -> bool {
    let t = s.strip_prefix(['+', '-']).unwrap_or(s);
    t.len() >= 2 && t.starts_with('0') && t.as_bytes()[1].is_ascii_digit()
}

/// Infer a schema for every column: type by unanimous vote of the non-null
/// cells (priority on ties, text as the fallback), `nullable` and
/// `nullTokens` from observed blanks and null-ish tokens. Indexed documents
/// scan a leading sample ([`INFER_SAMPLE_ROWS`]); a sample is evidence, not
/// certainty. Read-only; nothing is assigned until the user applies it.
pub fn infer_schema(doc: &Document) -> AppResult<DocumentSchema> {
    struct Acc {
        non_null: usize,
        blanks: usize,
        tokens: BTreeSet<String>,
        alive: [bool; INFER_CANDIDATES.len()],
    }
    let probes: Vec<ColumnSchema> = INFER_CANDIDATES
        .iter()
        .map(|&lt| ColumnSchema::new("", "", lt))
        .collect();
    let mut accs: Vec<Acc> = (0..doc.n_cols())
        .map(|_| Acc {
            non_null: 0,
            blanks: 0,
            tokens: BTreeSet::new(),
            alive: [true; INFER_CANDIDATES.len()],
        })
        .collect();

    let total = doc.n_rows();
    let scan = if doc.is_editable() {
        total
    } else {
        total.min(INFER_SAMPLE_ROWS)
    };
    doc.visit_rows(0..scan, &mut |_, row| {
        for (c, acc) in accs.iter_mut().enumerate() {
            let trimmed = row.get(c).map(String::as_str).unwrap_or("").trim();
            if trimmed.is_empty() {
                acc.blanks += 1;
                continue;
            }
            if NULLISH_TOKENS.contains(&trimmed) {
                acc.tokens.insert(trimmed.to_string());
                continue;
            }
            acc.non_null += 1;
            for (k, probe) in probes.iter().enumerate() {
                if !acc.alive[k] {
                    continue;
                }
                let ok = match probe.logical_type {
                    // Numeric candidates refuse superfluous leading zeros so
                    // code-like columns (ZIPs, account numbers) infer as text.
                    LogicalType::Integer | LogicalType::Decimal | LogicalType::Float => {
                        !has_leading_zero(trimmed) && parse_typed(trimmed, probe).is_ok()
                    }
                    _ => parse_typed(trimmed, probe).is_ok(),
                };
                if !ok {
                    acc.alive[k] = false;
                }
            }
        }
        Ok(true)
    })?;

    let mut columns = BTreeMap::new();
    for (c, acc) in accs.into_iter().enumerate() {
        let logical_type = if acc.non_null == 0 {
            LogicalType::Text
        } else {
            INFER_CANDIDATES
                .iter()
                .zip(acc.alive.iter())
                .find(|(_, &alive)| alive)
                .map(|(&lt, _)| lt)
                .unwrap_or(LogicalType::Text)
        };
        let mut schema = ColumnSchema::new(
            doc.column_ids()[c].clone(),
            doc.headers()[c].clone(),
            logical_type,
        );
        schema.nullable = acc.blanks > 0 || !acc.tokens.is_empty();
        schema.null_tokens = acc.tokens.into_iter().collect();
        columns.insert(schema.column_id.clone(), schema);
    }
    Ok(DocumentSchema { columns })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::parse::{parse, ParseSettings};

    fn probe(lt: LogicalType) -> ColumnSchema {
        ColumnSchema::new("c0", "col", lt)
    }

    fn probe_locale(lt: LogicalType, locale: &str) -> ColumnSchema {
        let mut s = probe(lt);
        s.locale = Some(locale.to_string());
        s
    }

    fn doc_from_csv(bytes: &[u8]) -> Document {
        let parsed = parse(bytes, &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn valid(schema: &ColumnSchema, raw: &str) -> TypedValue {
        match classify(Some(raw), schema) {
            CellState::Valid(v) => v,
            other => panic!("expected Valid for {raw:?}, got {other:?}"),
        }
    }

    fn invalid_reason(schema: &ColumnSchema, raw: &str) -> String {
        match classify(Some(raw), schema) {
            CellState::Invalid(reason) => reason,
            other => panic!("expected Invalid for {raw:?}, got {other:?}"),
        }
    }

    // ----- classification states ------------------------------------------

    #[test]
    fn five_way_classification_matrix() {
        let mut schema = probe(LogicalType::Integer);
        schema.null_tokens = vec!["NULL".to_string()];
        assert_eq!(classify(None, &schema), CellState::Missing);
        assert_eq!(classify(Some("NULL"), &schema), CellState::NullToken);
        assert_eq!(classify(Some(" NULL "), &schema), CellState::NullToken);
        assert_eq!(classify(Some(""), &schema), CellState::Empty);
        assert_eq!(classify(Some("   "), &schema), CellState::Empty);
        assert_eq!(
            classify(Some("42"), &schema),
            CellState::Valid(TypedValue::Integer(42))
        );
        assert!(matches!(
            classify(Some("abc"), &schema),
            CellState::Invalid(_)
        ));
    }

    #[test]
    fn empty_string_and_null_token_stay_distinguishable() {
        let mut schema = probe(LogicalType::Text);
        schema.null_tokens = vec!["N/A".to_string()];
        assert_eq!(classify(Some(""), &schema), CellState::Empty);
        assert_eq!(classify(Some("N/A"), &schema), CellState::NullToken);
        // An empty string CONFIGURED as a token classifies as the token.
        schema.null_tokens = vec![String::new()];
        assert_eq!(classify(Some(""), &schema), CellState::NullToken);
    }

    // ----- typed parsing per logical type ---------------------------------

    #[test]
    fn integer_valid_and_invalid() {
        let schema = probe(LogicalType::Integer);
        assert_eq!(valid(&schema, "0"), TypedValue::Integer(0));
        assert_eq!(valid(&schema, "+42"), TypedValue::Integer(42));
        assert_eq!(valid(&schema, "-7"), TypedValue::Integer(-7));
        // Leading zeros are numerically valid (inference vetoes them, but a
        // declared integer accepts them; conversion is the user's choice).
        assert_eq!(valid(&schema, "00501"), TypedValue::Integer(501));
        assert_eq!(valid(&schema, "1,234,567"), TypedValue::Integer(1_234_567));
        assert!(matches!(
            classify(Some("1.5"), &schema),
            CellState::Invalid(_)
        ));
        assert!(matches!(
            classify(Some("1e3"), &schema),
            CellState::Invalid(_)
        ));
        assert!(matches!(
            classify(Some("+"), &schema),
            CellState::Invalid(_)
        ));
    }

    #[test]
    fn integer_covers_i64_extremes_and_rejects_i128_overflow() {
        let schema = probe(LogicalType::Integer);
        assert_eq!(
            valid(&schema, "9223372036854775807"),
            TypedValue::Integer(i64::MAX as i128)
        );
        assert_eq!(
            valid(&schema, "-9223372036854775808"),
            TypedValue::Integer(i64::MIN as i128)
        );
        assert_eq!(
            valid(&schema, "170141183460469231731687303715884105727"),
            TypedValue::Integer(i128::MAX)
        );
        assert_eq!(
            invalid_reason(&schema, "170141183460469231731687303715884105728"),
            "integer out of range"
        );
    }

    #[test]
    fn decimal_preserves_precision_and_scale() {
        let schema = probe(LogicalType::Decimal);
        let v = valid(&schema, "1.50");
        assert_eq!(
            v,
            TypedValue::Decimal(DecimalValue {
                negative: false,
                digits: "150".to_string(),
                scale: 2
            })
        );
        if let TypedValue::Decimal(d) = v {
            assert_eq!(d.to_plain_string(), "1.50");
        }
        assert_eq!(
            valid(&schema, "-0.00"),
            TypedValue::Decimal(DecimalValue {
                negative: false,
                digits: "0".to_string(),
                scale: 2
            })
        );
        assert_eq!(
            valid(&schema, ".5"),
            TypedValue::Decimal(DecimalValue {
                negative: false,
                digits: "5".to_string(),
                scale: 1
            })
        );
        assert!(matches!(
            classify(Some("1.5e3"), &schema),
            CellState::Invalid(_)
        ));
        assert!(matches!(
            classify(Some("1..2"), &schema),
            CellState::Invalid(_)
        ));
    }

    #[test]
    fn float_valid_and_invalid() {
        let schema = probe(LogicalType::Float);
        assert_eq!(valid(&schema, "1.5e3"), TypedValue::Float(1500.0));
        assert_eq!(valid(&schema, "-2.5"), TypedValue::Float(-2.5));
        // Mirrors analyze::as_number: only finite values count.
        assert!(matches!(
            classify(Some("nan"), &schema),
            CellState::Invalid(_)
        ));
        assert!(matches!(
            classify(Some("inf"), &schema),
            CellState::Invalid(_)
        ));
        assert!(matches!(
            classify(Some("1e309"), &schema),
            CellState::Invalid(_)
        ));
    }

    #[test]
    fn boolean_accepts_exactly_the_documented_tokens() {
        let schema = probe(LogicalType::Boolean);
        assert_eq!(valid(&schema, "TRUE"), TypedValue::Boolean(true));
        assert_eq!(valid(&schema, "No"), TypedValue::Boolean(false));
        assert_eq!(valid(&schema, "1"), TypedValue::Boolean(true));
        assert_eq!(valid(&schema, "0"), TypedValue::Boolean(false));
        assert_eq!(valid(&schema, "yes"), TypedValue::Boolean(true));
        // y/n/t/f are detection heuristics (analyze), not schema booleans.
        assert!(matches!(
            classify(Some("y"), &schema),
            CellState::Invalid(_)
        ));
        assert!(matches!(
            classify(Some("t"), &schema),
            CellState::Invalid(_)
        ));
        assert!(matches!(
            classify(Some("maybe"), &schema),
            CellState::Invalid(_)
        ));
    }

    #[test]
    fn date_defaults_and_custom_input_formats() {
        let schema = probe(LogicalType::Date);
        assert_eq!(
            valid(&schema, "2024-01-31"),
            TypedValue::Date(NaiveDate::from_ymd_opt(2024, 1, 31).unwrap())
        );
        assert_eq!(
            valid(&schema, "01/31/2024"),
            TypedValue::Date(NaiveDate::from_ymd_opt(2024, 1, 31).unwrap())
        );
        assert!(matches!(
            classify(Some("2024-13-40"), &schema),
            CellState::Invalid(_)
        ));
        // Version-like codes must not read as dates (year guard).
        assert!(matches!(
            classify(Some("1.2.3"), &schema),
            CellState::Invalid(_)
        ));

        // Custom formats REPLACE the defaults.
        let mut custom = probe(LogicalType::Date);
        custom.input_formats = Some(vec!["%d.%m.%Y".to_string()]);
        assert_eq!(
            valid(&custom, "31.01.2024"),
            TypedValue::Date(NaiveDate::from_ymd_opt(2024, 1, 31).unwrap())
        );
        assert_eq!(
            invalid_reason(&custom, "2024-01-31"),
            "does not match any configured input format"
        );
    }

    #[test]
    fn datetime_defaults_offsets_and_midnight() {
        let schema = probe(LogicalType::Datetime);
        let expect = NaiveDate::from_ymd_opt(2024, 1, 31)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        assert_eq!(
            valid(&schema, "2024-01-31 10:00:00"),
            TypedValue::DateTime(expect)
        );
        // RFC 3339 with an offset normalises to UTC.
        let utc = NaiveDate::from_ymd_opt(2024, 1, 31)
            .unwrap()
            .and_hms_opt(8, 0, 0)
            .unwrap();
        assert_eq!(
            valid(&schema, "2024-01-31T10:00:00+02:00"),
            TypedValue::DateTime(utc)
        );
        // A plain date lands at midnight.
        let midnight = NaiveDate::from_ymd_opt(2024, 1, 31)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        assert_eq!(valid(&schema, "2024-01-31"), TypedValue::DateTime(midnight));
    }

    #[test]
    fn datetime_naive_values_resolve_in_schema_zone() {
        let mut schema = probe(LogicalType::Datetime);
        schema.time_zone = Some("Europe/Berlin".to_string());
        // June: CEST (UTC+2), so 12:00 wall time is 10:00 UTC.
        let utc = NaiveDate::from_ymd_opt(2024, 6, 1)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        assert_eq!(
            valid(&schema, "2024-06-01 12:00:00"),
            TypedValue::DateTime(utc)
        );
        // An explicit offset in the value still wins over the schema zone.
        let explicit = NaiveDate::from_ymd_opt(2024, 6, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        assert_eq!(
            valid(&schema, "2024-06-01T12:00:00Z"),
            TypedValue::DateTime(explicit)
        );
    }

    #[test]
    fn datetime_dst_fold_earliest_and_gap_invalid() {
        let mut schema = probe(LogicalType::Datetime);
        schema.time_zone = Some("America/New_York".to_string());
        // 2025-11-02 01:30 happens twice; the EARLIEST (EDT, UTC-4) wins.
        let fold = NaiveDate::from_ymd_opt(2025, 11, 2)
            .unwrap()
            .and_hms_opt(5, 30, 0)
            .unwrap();
        assert_eq!(
            valid(&schema, "2025-11-02 01:30:00"),
            TypedValue::DateTime(fold)
        );
        // 2025-03-09 02:30 does not exist (spring-forward gap).
        let reason = invalid_reason(&schema, "2025-03-09 02:30:00");
        assert!(reason.contains("nonexistent"), "{reason}");
    }

    #[test]
    fn datetime_unknown_zone_is_invalid() {
        let mut schema = probe(LogicalType::Datetime);
        schema.time_zone = Some("Mars/Olympus_Mons".to_string());
        let reason = invalid_reason(&schema, "2024-06-01 12:00:00");
        assert!(reason.contains("unknown time zone"), "{reason}");
    }

    #[test]
    fn uuid_canonicalises_to_lowercase() {
        let schema = probe(LogicalType::Uuid);
        assert_eq!(
            valid(&schema, "6FA459EA-EE8A-3CA4-894E-DB77E160355E"),
            TypedValue::Uuid("6fa459ea-ee8a-3ca4-894e-db77e160355e".to_string())
        );
        assert!(matches!(
            classify(Some("6fa459eaee8a3ca4894edb77e160355e"), &schema),
            CellState::Invalid(_)
        ));
    }

    #[test]
    fn json_validity_via_serde() {
        let schema = probe(LogicalType::Json);
        assert!(matches!(
            classify(Some(r#"{"a": [1, 2]}"#), &schema),
            CellState::Valid(TypedValue::Json(_))
        ));
        // Bare scalars are valid JSON documents.
        assert!(matches!(
            classify(Some("123"), &schema),
            CellState::Valid(TypedValue::Json(_))
        ));
        assert!(matches!(
            classify(Some("{a: 1}"), &schema),
            CellState::Invalid(_)
        ));
    }

    // ----- locale-aware numbers -------------------------------------------

    #[test]
    fn locale_decimal_separators() {
        let de = probe_locale(LogicalType::Decimal, "de-DE");
        assert_eq!(
            valid(&de, "1.234,56"),
            TypedValue::Decimal(DecimalValue {
                negative: false,
                digits: "123456".to_string(),
                scale: 2
            })
        );
        assert_eq!(
            valid(&de, "0,5"),
            TypedValue::Decimal(DecimalValue {
                negative: false,
                digits: "5".to_string(),
                scale: 1
            })
        );
        // "1.5" is NOT 15 under de-DE: groups must be exactly three digits.
        assert_eq!(invalid_reason(&de, "1.5"), "misplaced grouping separator");

        let fr = probe_locale(LogicalType::Decimal, "fr-FR");
        assert_eq!(
            valid(&fr, "1 234,5"),
            TypedValue::Decimal(DecimalValue {
                negative: false,
                digits: "12345".to_string(),
                scale: 1
            })
        );
        assert_eq!(
            valid(&fr, "1\u{00A0}234,5"),
            TypedValue::Decimal(DecimalValue {
                negative: false,
                digits: "12345".to_string(),
                scale: 1
            })
        );

        let ch = probe_locale(LogicalType::Decimal, "de-CH");
        assert_eq!(
            valid(&ch, "1'234.50"),
            TypedValue::Decimal(DecimalValue {
                negative: false,
                digits: "123450".to_string(),
                scale: 2
            })
        );

        let en = probe_locale(LogicalType::Decimal, "en-US");
        assert_eq!(
            valid(&en, "1,234,567.89"),
            TypedValue::Decimal(DecimalValue {
                negative: false,
                digits: "123456789".to_string(),
                scale: 2
            })
        );
    }

    #[test]
    fn locale_grouping_applies_to_integers() {
        let de = probe_locale(LogicalType::Integer, "de-DE");
        assert_eq!(valid(&de, "1.234"), TypedValue::Integer(1234));
        let fr = probe_locale(LogicalType::Integer, "fr-FR");
        assert_eq!(valid(&fr, "1 234 567"), TypedValue::Integer(1_234_567));
        // Grouping after the decimal point is never valid.
        let en = probe_locale(LogicalType::Decimal, "en-US");
        assert!(matches!(
            classify(Some("1.23,456"), &en),
            CellState::Invalid(_)
        ));
    }

    // ----- text fidelity ---------------------------------------------------

    #[test]
    fn text_declared_column_preserves_leading_zeroes() {
        let schema = probe(LogicalType::Text);
        // A ZIP column declared text: the value is valid AS-IS and canonical
        // conversion leaves it untouched.
        assert_eq!(
            valid(&schema, "00501"),
            TypedValue::Text("00501".to_string())
        );
        assert_eq!(canonical_text(&schema, "00501"), Some("00501".to_string()));
    }

    // ----- validation ------------------------------------------------------

    #[test]
    fn validate_value_nullable_rules() {
        let mut schema = probe(LogicalType::Integer);
        schema.null_tokens = vec!["NULL".to_string()];
        schema.nullable = true;
        assert!(validate_value(&schema, "42").is_ok());
        assert!(validate_value(&schema, "").is_ok());
        assert!(validate_value(&schema, "NULL").is_ok());

        schema.nullable = false;
        assert!(validate_value(&schema, "42").is_ok());
        assert!(validate_value(&schema, "").is_err());
        assert!(validate_value(&schema, "NULL").is_err());

        // Empty string is a valid TEXT value even when not nullable.
        let mut text = probe(LogicalType::Text);
        text.nullable = false;
        assert!(validate_value(&text, "").is_ok());
    }

    #[test]
    fn validate_value_reports_type_errors() {
        let schema = probe(LogicalType::Integer);
        let err = validate_value(&schema, "not a number").unwrap_err();
        assert!(err.contains("unexpected character"), "{err}");
        assert!(validate_value(&probe(LogicalType::Date), "2024-13-40").is_err());
    }

    // ----- canonical conversion text --------------------------------------

    #[test]
    fn canonical_text_forms() {
        assert_eq!(
            canonical_text(&probe(LogicalType::Integer), "007"),
            Some("7".to_string())
        );
        assert_eq!(
            canonical_text(&probe_locale(LogicalType::Decimal, "de-DE"), "1.234,50"),
            Some("1234.50".to_string())
        );
        assert_eq!(
            canonical_text(&probe(LogicalType::Boolean), " YES "),
            Some("true".to_string())
        );
        assert_eq!(
            canonical_text(&probe(LogicalType::Date), "01/31/2024"),
            Some("2024-01-31".to_string())
        );
        assert_eq!(
            canonical_text(&probe(LogicalType::Datetime), "2024-01-31 10:00"),
            Some("2024-01-31T10:00:00".to_string())
        );
        assert_eq!(
            canonical_text(
                &probe(LogicalType::Uuid),
                "6FA459EA-EE8A-3CA4-894E-DB77E160355E"
            ),
            Some("6fa459ea-ee8a-3ca4-894e-db77e160355e".to_string())
        );
        // Invalid, empty and null-token cells have no canonical form.
        assert_eq!(canonical_text(&probe(LogicalType::Integer), "abc"), None);
        assert_eq!(canonical_text(&probe(LogicalType::Integer), ""), None);
    }

    // ----- display formatting ---------------------------------------------

    #[test]
    fn format_integer_patterns() {
        let mut schema = probe(LogicalType::Integer);
        schema.display_format = Some("thousands".to_string());
        assert_eq!(
            format_value(&schema, "9223372036854775807"),
            "9,223,372,036,854,775,807"
        );
        assert_eq!(format_value(&schema, "-1234"), "-1,234");
        schema.locale = Some("de-DE".to_string());
        assert_eq!(format_value(&schema, "1234567"), "1.234.567");
        schema.locale = None;
        schema.display_format = Some("fixed:2".to_string());
        assert_eq!(format_value(&schema, "42"), "42.00");
        schema.display_format = Some("percent".to_string());
        assert_eq!(format_value(&schema, "42"), "4200%");
    }

    #[test]
    fn format_decimal_patterns_and_rounding() {
        let mut schema = probe(LogicalType::Decimal);
        schema.display_format = Some("fixed:2".to_string());
        // Exact string arithmetic: 2.675 → 2.68 (an f64 would give 2.67).
        assert_eq!(format_value(&schema, "2.675"), "2.68");
        assert_eq!(format_value(&schema, "-2.675"), "-2.68");
        assert_eq!(format_value(&schema, "1.005"), "1.01");
        assert_eq!(format_value(&schema, "0.005"), "0.01");
        assert_eq!(format_value(&schema, "1.5"), "1.50");
        schema.display_format = Some("percent".to_string());
        assert_eq!(format_value(&schema, "0.125"), "12.5%");
        schema.display_format = Some("thousands".to_string());
        schema.locale = Some("de-DE".to_string());
        assert_eq!(format_value(&schema, "1234567,5"), "1.234.567,5");
    }

    #[test]
    fn format_float_patterns() {
        let mut schema = probe(LogicalType::Float);
        schema.display_format = Some("fixed:2".to_string());
        assert_eq!(format_value(&schema, "1.5"), "1.50");
        schema.display_format = Some("thousands".to_string());
        assert_eq!(format_value(&schema, "1234.5"), "1,234.5");
        schema.display_format = Some("percent".to_string());
        assert_eq!(format_value(&schema, "0.5"), "50%");
    }

    #[test]
    fn format_date_patterns() {
        let mut date = probe(LogicalType::Date);
        date.display_format = Some("eu".to_string());
        assert_eq!(format_value(&date, "2024-01-31"), "31.01.2024");
        date.display_format = Some("us".to_string());
        assert_eq!(format_value(&date, "2024-01-31"), "01/31/2024");
        date.display_format = Some("long".to_string());
        assert_eq!(format_value(&date, "2024-01-31"), "Jan 31, 2024");

        let mut dt = probe(LogicalType::Datetime);
        dt.display_format = Some("iso".to_string());
        assert_eq!(
            format_value(&dt, "2024-01-31T10:00:00Z"),
            "2024-01-31 10:00:00"
        );
    }

    #[test]
    fn format_never_touches_unformattable_cells() {
        let mut schema = probe(LogicalType::Integer);
        schema.null_tokens = vec!["NULL".to_string()];
        schema.display_format = Some("thousands".to_string());
        // Invalid, empty and null-token cells pass through verbatim.
        assert_eq!(format_value(&schema, "abc"), "abc");
        assert_eq!(format_value(&schema, ""), "");
        assert_eq!(format_value(&schema, "NULL"), "NULL");
        // Unknown patterns are ignored.
        schema.display_format = Some("scientific".to_string());
        assert_eq!(format_value(&schema, "1234"), "1234");
        // No pattern at all: raw text.
        schema.display_format = None;
        assert_eq!(format_value(&schema, "1234"), "1234");
    }

    // ----- inference -------------------------------------------------------

    #[test]
    fn inference_sanity_over_a_mixed_document() {
        let doc = doc_from_csv(
            b"zip,count,price,flag,when,id,note\n\
              00501,1,1.50,true,2024-01-01,6fa459ea-ee8a-3ca4-894e-db77e160355e,hello\n\
              02134,2,2.75,false,2024-02-01,16fd2706-8baf-433b-82eb-8c7fada847da,NULL\n\
              ,3,3.00,true,2024-03-01,6fa459ea-ee8a-3ca4-894e-db77e160355e,world\n",
        );
        let schema = infer_schema(&doc).unwrap();
        let by_id = |id: &str| schema.column(id).unwrap();

        // ZIP: leading zeroes veto integer; blank in row 3 → nullable.
        assert_eq!(by_id("c0").logical_type, LogicalType::Text);
        assert!(by_id("c0").nullable);
        assert_eq!(by_id("c1").logical_type, LogicalType::Integer);
        assert!(!by_id("c1").nullable);
        assert_eq!(by_id("c2").logical_type, LogicalType::Decimal);
        assert_eq!(by_id("c3").logical_type, LogicalType::Boolean);
        assert_eq!(by_id("c4").logical_type, LogicalType::Date);
        assert_eq!(by_id("c5").logical_type, LogicalType::Uuid);
        // note: "NULL" is observed as a null token, the rest is text.
        assert_eq!(by_id("c6").logical_type, LogicalType::Text);
        assert!(by_id("c6").nullable);
        assert_eq!(by_id("c6").null_tokens, vec!["NULL".to_string()]);
        // Names and modes fill in sensibly.
        assert_eq!(by_id("c2").name, "price");
        assert_eq!(by_id("c2").validation_mode, ValidationMode::Advisory);
    }

    #[test]
    fn inference_priorities() {
        let doc = doc_from_csv(
            b"flags,sci,stamps\n\
              1,1e3,2024-01-01 10:00:00\n\
              0,2.5,2024-01-02\n\
              1,3,2024-01-03 11:30:00\n",
        );
        let schema = infer_schema(&doc).unwrap();
        // 0/1 columns stay integer (analyze excludes 0/1 from booleans too).
        assert_eq!(
            schema.column("c0").unwrap().logical_type,
            LogicalType::Integer
        );
        // Exponent notation forces float over decimal.
        assert_eq!(
            schema.column("c1").unwrap().logical_type,
            LogicalType::Float
        );
        // Mixed date + datetime cells: datetime wins, date dies.
        assert_eq!(
            schema.column("c2").unwrap().logical_type,
            LogicalType::Datetime
        );
    }

    // ----- stable IDs across structural edits ------------------------------

    #[test]
    fn schema_keyed_by_column_id_survives_rename_and_move() {
        let mut doc = doc_from_csv(b"a,b,c\n1,x,2.5\n2,y,3.5\n");
        let schema = infer_schema(&doc).unwrap();
        assert_eq!(
            schema.column("c0").unwrap().logical_type,
            LogicalType::Integer
        );

        doc.rename_column(0, "renamed".to_string()).unwrap();
        doc.move_column(0, 2).unwrap();
        // The integer column now sits at position 2 under a new name, but its
        // ID — and therefore its schema entry — is unchanged.
        assert_eq!(doc.column_ids()[2], "c0");
        assert_eq!(doc.headers()[2], "renamed");
        assert_eq!(
            schema.column("c0").unwrap().logical_type,
            LogicalType::Integer
        );
    }

    // ----- import / export -------------------------------------------------

    fn sample_schema() -> DocumentSchema {
        let mut schema = DocumentSchema::default();
        let mut a = ColumnSchema::new("c0", "amount", LogicalType::Decimal);
        a.nullable = false;
        a.null_tokens = vec!["NULL".to_string()];
        a.locale = Some("de-DE".to_string());
        a.display_format = Some("fixed:2".to_string());
        a.validation_mode = ValidationMode::Strict;
        schema.set_column(a);
        let mut b = ColumnSchema::new("c1", "when", LogicalType::Datetime);
        b.time_zone = Some("Europe/Berlin".to_string());
        b.input_formats = Some(vec!["%d.%m.%Y %H:%M".to_string()]);
        schema.set_column(b);
        schema
    }

    #[test]
    fn export_import_round_trip() {
        let schema = sample_schema();
        let order = vec!["c1".to_string(), "c0".to_string()];
        let json = export_to_json(&schema, &order).unwrap();
        let restored = import_from_json(&json).unwrap();
        assert_eq!(restored, schema);
        // Column order in the envelope follows the document order.
        let export = export_schema(&schema, &order);
        assert_eq!(export.version, SCHEMA_VERSION);
        assert_eq!(export.columns[0].column_id, "c1");
        assert_eq!(export.columns[1].column_id, "c0");
    }

    #[test]
    fn import_rejects_unknown_version() {
        let err = import_from_json(r#"{"version": 99, "columns": []}"#).unwrap_err();
        assert!(
            err.to_string().contains("unsupported schema version 99"),
            "{err}"
        );
        // The version check fires even when the column shape is unreadable.
        let err = import_from_json(r#"{"version": 2, "columns": "future-format"}"#).unwrap_err();
        assert!(
            err.to_string().contains("unsupported schema version 2"),
            "{err}"
        );
    }

    #[test]
    fn import_rejects_duplicates_and_garbage() {
        let json = r#"{"version": 1, "columns": [
            {"columnId": "c0", "name": "a", "logicalType": "text"},
            {"columnId": "c0", "name": "b", "logicalType": "integer"}
        ]}"#;
        let err = import_from_json(json).unwrap_err();
        assert!(err.to_string().contains("duplicate columnId"), "{err}");
        assert!(import_from_json("not json at all").is_err());
    }

    #[test]
    fn wire_format_is_camel_case() {
        let schema = sample_schema();
        let json = export_to_json(&schema, &["c0".to_string(), "c1".to_string()]).unwrap();
        for key in [
            "\"columnId\"",
            "\"logicalType\"",
            "\"nullTokens\"",
            "\"validationMode\"",
            "\"timeZone\"",
            "\"inputFormats\"",
            "\"displayFormat\"",
            "\"version\"",
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
        assert!(json.contains("\"datetime\""));
        assert!(json.contains("\"strict\""));
        // Optional fields that are unset are omitted, not null.
        assert!(!json.contains("null,"));
    }

    // ----- typed ordering ---------------------------------------------------

    #[test]
    fn decimal_cmp_value_is_exact_across_scales() {
        let schema = probe(LogicalType::Decimal);
        let dec = |s: &str| match valid(&schema, s) {
            TypedValue::Decimal(d) => d,
            other => panic!("expected decimal, got {other:?}"),
        };
        use std::cmp::Ordering::*;
        assert_eq!(dec("1.50").cmp_value(&dec("1.5")), Equal);
        assert_eq!(dec("1.49").cmp_value(&dec("1.5")), Less);
        assert_eq!(dec("-1.5").cmp_value(&dec("-1.49")), Less);
        assert_eq!(dec("-0.00").cmp_value(&dec("0")), Equal);
        assert_eq!(dec("10").cmp_value(&dec("9.999999")), Greater);
        assert_eq!(dec("-3").cmp_value(&dec("2")), Less);
        // Beyond f64 precision: differs only in the 18th digit.
        assert_eq!(
            dec("0.123456789012345678").cmp_value(&dec("0.123456789012345679")),
            Less
        );
    }

    #[test]
    fn compare_cells_ranks_nullish_valid_invalid() {
        let mut schema = probe(LogicalType::Integer);
        schema.null_tokens = vec!["NULL".to_string()];
        use std::cmp::Ordering::*;
        // Null-ish first, then valid (typed), then invalid.
        assert_eq!(compare_cells(&schema, "", "5"), Less);
        assert_eq!(compare_cells(&schema, "NULL", "5"), Less);
        assert_eq!(compare_cells(&schema, "5", "abc"), Less);
        assert_eq!(compare_cells(&schema, "9", "10"), Less, "typed, not text");
        // Within a band, raw text breaks ties deterministically.
        assert_eq!(compare_cells(&schema, "", "NULL"), Less);
        assert_eq!(compare_cells(&schema, "abc", "abd"), Less);
        // Equal typed values with different text still order deterministically.
        assert_eq!(compare_cells(&schema, "007", "7"), Less);
    }

    #[test]
    fn numeric_and_temporal_cell_helpers() {
        let mut schema = probe_locale(LogicalType::Decimal, "de-DE");
        schema.null_tokens = vec!["NULL".to_string()];
        assert!(matches!(
            numeric_cell(&schema, "1.234,5"),
            NumericCell::Value(v) if v == 1234.5
        ));
        assert!(matches!(numeric_cell(&schema, "NULL"), NumericCell::Null));
        assert!(matches!(numeric_cell(&schema, ""), NumericCell::Null));
        assert!(matches!(numeric_cell(&schema, "abc"), NumericCell::Invalid));

        let date = probe(LogicalType::Date);
        let midnight = NaiveDate::from_ymd_opt(2024, 1, 31)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        assert_eq!(temporal_cell(&date, "2024-01-31"), Some(midnight));
        assert_eq!(temporal_cell(&date, "nope"), None);
    }

    // ----- schema input validation + issues ---------------------------------

    #[test]
    fn validate_column_schema_rejects_bad_inputs() {
        let good = probe(LogicalType::Integer);
        assert!(validate_column_schema(&good).is_ok());
        let mut bad_tz = probe(LogicalType::Datetime);
        bad_tz.time_zone = Some("Mars/Olympus_Mons".to_string());
        assert!(validate_column_schema(&bad_tz).is_err());
        let mut ok_tz = probe(LogicalType::Datetime);
        ok_tz.time_zone = Some("Europe/Berlin".to_string());
        assert!(validate_column_schema(&ok_tz).is_ok());
        let mut bad_fmt = probe(LogicalType::Date);
        bad_fmt.input_formats = Some(vec!["%Y".to_string(), "  ".to_string()]);
        assert!(validate_column_schema(&bad_fmt).is_err());
        let mut empty_id = probe(LogicalType::Text);
        empty_id.column_id = String::new();
        assert!(validate_column_schema(&empty_id).is_err());
        // Unknown display formats are LEGAL (ignored at render time).
        let mut odd_fmt = probe(LogicalType::Integer);
        odd_fmt.display_format = Some("no-such-pattern".to_string());
        assert!(validate_column_schema(&odd_fmt).is_ok());
    }

    #[test]
    fn schema_issue_truncates_long_values() {
        let long = "x".repeat(1000);
        let issue = SchemaIssue::new(1, 2, "c0", &long, "reason", 7);
        assert!(issue.value.chars().count() <= 201, "200 chars + ellipsis");
        assert!(issue.value.ends_with('…'));
        let short = SchemaIssue::new(1, 2, "c0", "ok", "reason", 7);
        assert_eq!(short.value, "ok");
    }

    #[test]
    fn column_kind_mapping_is_coarse() {
        use crate::dto::ColumnKind;
        assert_eq!(column_kind_of(LogicalType::Integer), ColumnKind::Number);
        assert_eq!(column_kind_of(LogicalType::Decimal), ColumnKind::Number);
        assert_eq!(column_kind_of(LogicalType::Float), ColumnKind::Number);
        assert_eq!(column_kind_of(LogicalType::Boolean), ColumnKind::Bool);
        assert_eq!(column_kind_of(LogicalType::Date), ColumnKind::Date);
        assert_eq!(column_kind_of(LogicalType::Datetime), ColumnKind::Date);
        assert_eq!(column_kind_of(LogicalType::Uuid), ColumnKind::Text);
        assert_eq!(column_kind_of(LogicalType::Json), ColumnKind::Text);
    }
}
