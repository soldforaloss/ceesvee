//! F33: JSON / JSON Lines import engine.
//!
//! Turns structured JSON into tabular documents without a pre-conversion to
//! CSV. Four input shapes are recognised:
//!
//! * a JSON array of objects (`[{...}, {...}]`) — keys become columns;
//! * a JSON array of arrays (`[[...], [...]]`) — positions become columns
//!   named `Column 1`, `Column 2`, … (the headerless-CSV convention);
//! * JSON Lines / NDJSON — one JSON value per physical line, streamed with
//!   bounded memory through a byte-offset record index ([`JsonlIndex`],
//!   mirroring the F10 CSV record index: one `u64` line-start offset per
//!   record, windowed reads by seek);
//! * a JSON object containing a record array at a JSON Pointer (RFC 6901) —
//!   candidates are auto-detected for documents up to [`DOC_PARSE_LIMIT`];
//!   larger documents need an explicit pointer, which is then resolved by a
//!   STREAMING walk (the surrounding document is never materialised).
//!
//! ## Documented rules (the parts other stages rely on)
//!
//! **Key union (column order).** Records may carry heterogeneous key sets;
//! the imported column set is their deterministic union: every flattened
//! path is appended the first time it is seen while scanning records in
//! order. Paths introduced by the FIRST record append in document order
//! (object key order as written in the file — [`JVal`] preserves it); paths
//! first introduced by a LATER record append at the tail in alphabetical
//! order (per record batch). The union is computed in a dedicated scan pass,
//! so the schema is complete before any row is emitted (the
//! [`crate::tabular`] fixed-schema contract).
//!
//! **Path names and escaping.** Nested objects flatten to dot-joined paths
//! (`address.city`). A literal `.` inside a real key escapes as `\.` and a
//! literal backslash as `\\`, so `{"a.b": 1}` and `{"a": {"b": 2}}` yield
//! the distinct columns `a\.b` and `a.b`. [`split_path`] reverses the
//! escaping (the export stage rebuilds nested objects from it). Duplicate
//! keys within one JSON object follow last-wins (matching serde_json/JS).
//!
//! **Missing vs explicit null.** A missing property is NOT the same thing
//! as `"key": null`, and the distinction survives into the document through
//! two per-import tokens:
//! * explicit JSON `null` → the `nullToken` cell text (default `"null"`),
//!   registered as a schema null token on every column that contained one,
//!   so [`crate::schema::classify`] reports `NullToken`;
//! * a missing property → the `missingToken` cell text (default `""`),
//!   which classifies as `Empty` and is what the F33 export maps back to an
//!   omitted key.
//! The two tokens must differ. String values that literally equal either
//! token are counted and surfaced as preview warnings (with the default
//! `missingToken` of `""`, a present-but-empty JSON string is one of those
//! collisions); pick distinct tokens when the distinction matters for your
//! data. At the [`crate::tabular::TabularRow`] layer the same states map to
//! `None` (missing) vs `Some(token)` (null) vs `Some("")` (empty).
//!
//! **Nested-object policies.** `flatten` (path columns, the default),
//! `preserveJson` (the subtree as compact JSON text, key order preserved),
//! and `ignorePaths` (drop the listed flattened paths and everything under
//! them, whatever the policy). An empty object flattens to no cells (its
//! subtree is missing).
//!
//! **Array policies.** `preserveJson` (default; compact JSON text),
//! `join` (primitives joined with `joinSeparator`, nulls rendered as the
//! null token; arrays containing objects/arrays reject), `reject` (any
//! array value fails the import), and `explode` (one output row per
//! element; element objects flatten into `path.*` columns). A record whose
//! explosion involves TWO OR MORE array fields requires an explicit
//! `multiArray` choice: `cartesian` (cross product, last field varies
//! fastest) or `zip` (index-aligned to the longest array, shorter arrays
//! pad as missing). An empty exploded array produces one row with the field
//! missing (records are never silently dropped). Arrays nested inside an
//! exploded element cannot themselves explode (choose preserve/join).
//!
//! **Numbers.** JSON numbers normalise through serde_json (i64/u64/f64):
//! `1.0` stays `1.0`, but literal forms like `1e2` render as `100.0` —
//! IEEE-double semantics, same as JavaScript.
//!
//! **Errors.** Invalid JSON reports the absolute byte offset, 1-based line
//! and column, and a short context snippet. The scan pass validates the
//! whole input BEFORE any document is built, so invalid input can never
//! produce a partially opened document; the emit pass streams into a
//! [`DerivedDocumentBuilder`], whose guard cleans up on any error.
//!
//! **Memory.** JSON Lines and root/pointer arrays stream in two passes
//! (scan, then emit); rows accumulate through the derived-document builder,
//! which spills to a guarded temp CSV past [`SPILL_BUDGET`] and finishes as
//! an INDEXED read-only document over the F10 machinery (`forceIndexed`
//! spills immediately). Only object-document candidate auto-detection
//! parses a whole file, and only up to [`DOC_PARSE_LIMIT`].

use std::cmp::Reverse;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde::de::{DeserializeSeed, Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::derived::{DerivedDocumentBuilder, SPILL_BUDGET};
use crate::document::Document;
use crate::dto::FileFingerprint;
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;
use crate::schema::{ColumnSchema, LogicalType};
use crate::util;

/// Bytes sniffed from the head of the file for shape detection.
const HEAD_PROBE: usize = 64 * 1024;
/// Chunk size for the streaming JSONL framer (tests shrink it to force
/// records to straddle chunk boundaries).
const JSONL_CHUNK: usize = 256 * 1024;
/// Largest file the OBJECT-DOCUMENT candidate auto-detection will fully
/// parse (streaming paths have no size limit).
pub const DOC_PARSE_LIMIT: u64 = 128 * 1024 * 1024;
/// Hard cap on imported columns (heterogeneous keys can otherwise explode).
pub const MAX_COLUMNS: usize = 10_000;
/// Sample rows retained by the preview scan.
pub const SAMPLE_ROWS: usize = 50;
/// Cap on reported nested-object / array-field paths in the preview.
const REPORT_LIMIT: usize = 200;
/// Candidate record arrays reported, and the depth they are searched to.
const MAX_CANDIDATES: usize = 32;
const CANDIDATE_DEPTH: usize = 8;
/// Bytes of context shown on each side of an error location.
const CONTEXT_RADIUS: usize = 24;
/// Cooperative-cancellation check intervals (records in the scan pass,
/// emitted rows in the import pass).
const CANCEL_EVERY_RECORDS: u64 = 256;
const CANCEL_EVERY_ROWS: u64 = 1024;

const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

// ----- options ---------------------------------------------------------------------

/// How nested objects are handled (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NestedPolicy {
    #[default]
    Flatten,
    PreserveJson,
}

/// How array values are handled (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArrayPolicy {
    #[default]
    PreserveJson,
    Explode,
    Join,
    Reject,
}

/// The explicit choice required when a record explodes MORE THAN ONE array
/// field (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MultiArrayMode {
    Cartesian,
    Zip,
}

/// Everything an import needs to know (wire DTO, camelCase).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct JsonImportOptions {
    /// Record-array JSON Pointer; `None`/`""` is the document root. Does not
    /// apply to JSON Lines input.
    pub pointer: Option<String>,
    pub nested_policy: NestedPolicy,
    /// Flattened paths dropped together with everything under them.
    pub ignore_paths: Vec<String>,
    pub array_policy: ArrayPolicy,
    /// Required when `array_policy` is `join`.
    pub join_separator: Option<String>,
    /// Required when a record explodes two or more array fields.
    pub multi_array: Option<MultiArrayMode>,
    /// Cell text for explicit JSON null (module docs).
    pub null_token: String,
    /// Cell text for a missing property (module docs).
    pub missing_token: String,
    /// Spill to the indexed read-only backing immediately instead of
    /// size-based auto-selection.
    pub force_indexed: bool,
}

impl Default for JsonImportOptions {
    fn default() -> JsonImportOptions {
        JsonImportOptions {
            pointer: None,
            nested_policy: NestedPolicy::default(),
            ignore_paths: Vec::new(),
            array_policy: ArrayPolicy::default(),
            join_separator: None,
            multi_array: None,
            null_token: "null".into(),
            missing_token: String::new(),
            force_indexed: false,
        }
    }
}

/// Validated, lookup-friendly form of the options.
struct Resolved {
    nested: NestedPolicy,
    array: ArrayPolicy,
    join_sep: String,
    multi: Option<MultiArrayMode>,
    ignore: HashSet<String>,
    null_token: String,
    missing_token: String,
}

impl JsonImportOptions {
    fn resolve(&self) -> AppResult<Resolved> {
        if self.null_token == self.missing_token {
            return Err(AppError::invalid(format!(
                "the null token and the missing-value text are both {:?}; they must differ so \
                 explicit nulls stay distinguishable from missing fields",
                self.null_token
            )));
        }
        let join_sep = match (self.array_policy, &self.join_separator) {
            (ArrayPolicy::Join, Some(sep)) => sep.clone(),
            (ArrayPolicy::Join, None) => {
                return Err(AppError::invalid(
                    "the join array policy needs a separator (joinSeparator)",
                ))
            }
            _ => String::new(),
        };
        Ok(Resolved {
            nested: self.nested_policy,
            array: self.array_policy,
            join_sep,
            multi: self.multi_array,
            ignore: self.ignore_paths.iter().cloned().collect(),
            null_token: self.null_token.clone(),
            missing_token: self.missing_token.clone(),
        })
    }
}

// ----- an order-preserving JSON value ----------------------------------------------

/// Like `serde_json::Value`, but objects keep their keys in DOCUMENT order
/// (the key-union rule depends on it) without flipping serde_json's global
/// `preserve_order` feature for the whole app.
#[derive(Debug, Clone, PartialEq)]
pub enum JVal {
    Null,
    Bool(bool),
    Num(serde_json::Number),
    Str(String),
    Arr(Vec<JVal>),
    Obj(Vec<(String, JVal)>),
}

impl<'de> Deserialize<'de> for JVal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<JVal, D::Error> {
        struct JVisitor;
        impl<'de> Visitor<'de> for JVisitor {
            type Value = JVal;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_unit<E>(self) -> Result<JVal, E> {
                Ok(JVal::Null)
            }

            fn visit_bool<E>(self, v: bool) -> Result<JVal, E> {
                Ok(JVal::Bool(v))
            }

            fn visit_i64<E>(self, v: i64) -> Result<JVal, E> {
                Ok(JVal::Num(v.into()))
            }

            fn visit_u64<E>(self, v: u64) -> Result<JVal, E> {
                Ok(JVal::Num(v.into()))
            }

            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<JVal, E> {
                serde_json::Number::from_f64(v)
                    .map(JVal::Num)
                    .ok_or_else(|| E::custom("non-finite numbers are not valid JSON"))
            }

            fn visit_str<E>(self, v: &str) -> Result<JVal, E> {
                Ok(JVal::Str(v.to_owned()))
            }

            fn visit_string<E>(self, v: String) -> Result<JVal, E> {
                Ok(JVal::Str(v))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<JVal, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(JVal::Arr(items))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<JVal, A::Error> {
                let mut entries = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, JVal>()? {
                    entries.push((key, value));
                }
                Ok(JVal::Obj(entries))
            }
        }
        deserializer.deserialize_any(JVisitor)
    }
}

impl Serialize for JVal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            JVal::Null => serializer.serialize_unit(),
            JVal::Bool(b) => serializer.serialize_bool(*b),
            JVal::Num(n) => n.serialize(serializer),
            JVal::Str(s) => serializer.serialize_str(s),
            JVal::Arr(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            JVal::Obj(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (k, v) in entries {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

fn compact(v: &JVal) -> String {
    serde_json::to_string(v).expect("JVal serialization cannot fail")
}

fn is_primitive(v: &JVal) -> bool {
    !matches!(v, JVal::Obj(_) | JVal::Arr(_))
}

// ----- path escaping ---------------------------------------------------------------

/// Escape one object key for use as a path segment: `.` → `\.`, `\` → `\\`.
pub fn escape_key(key: &str) -> String {
    if !key.contains(['.', '\\']) {
        return key.to_string();
    }
    let mut out = String::with_capacity(key.len() + 2);
    for c in key.chars() {
        if c == '.' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn join_path(base: &str, key: &str) -> String {
    if base.is_empty() {
        escape_key(key)
    } else {
        format!("{base}.{}", escape_key(key))
    }
}

/// Split a flattened path back into its original key segments, undoing
/// [`escape_key`]. The export stage rebuilds nested objects with this.
pub fn split_path(path: &str) -> Vec<String> {
    let mut segments = vec![String::new()];
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(escaped) = chars.next() {
                    segments.last_mut().expect("non-empty").push(escaped);
                }
            }
            '.' => segments.push(String::new()),
            _ => segments.last_mut().expect("non-empty").push(c),
        }
    }
    segments
}

// ----- shape detection -------------------------------------------------------------

/// The recognised input shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DetectedShape {
    ObjectArray,
    ArrayOfArrays,
    PrimitiveArray,
    JsonLines,
    ObjectDocument,
    ScalarDocument,
}

/// One auto-detected record-array candidate inside an object document.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerCandidate {
    /// RFC 6901 JSON Pointer (`""` is the root).
    pub pointer: String,
    pub records: u64,
    /// `"object"`, `"array"`, `"primitive"`, `"mixed"` or `"empty"`.
    pub element_kind: String,
}

/// Result of sniffing a file's shape.
#[derive(Debug, Clone)]
pub struct ShapeInfo {
    pub shape: DetectedShape,
    /// Ranked record-array candidates (object documents only).
    pub candidates: Vec<PointerCandidate>,
    /// Set when candidates could not be computed (file too large).
    pub note: Option<String>,
}

/// Sniff `path` and classify its shape; object documents up to
/// [`DOC_PARSE_LIMIT`] also get ranked pointer candidates.
///
/// Heuristics: the `.jsonl` / `.ndjson` extensions always mean JSON Lines.
/// Otherwise a leading `[` is an array document, and a leading `{` is JSON
/// Lines exactly when the first physical line is complete JSON on its own
/// AND more non-blank content follows (a single-line object file without
/// the extension detects as an object document).
pub fn detect_shape(path: &Path) -> AppResult<ShapeInfo> {
    detect_shape_inner(path, true)
}

fn detect_shape_inner(path: &Path, want_candidates: bool) -> AppResult<ShapeInfo> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut head = vec![0u8; HEAD_PROBE];
    let mut len = 0usize;
    while len < head.len() {
        let n = file.read(&mut head[len..])?;
        if n == 0 {
            break;
        }
        len += n;
    }
    head.truncate(len);
    if head.is_empty() {
        return Err(AppError::invalid("the file is empty"));
    }
    if head.starts_with(&[0xFF, 0xFE])
        || head.starts_with(&[0xFE, 0xFF])
        || head.iter().take(256).any(|&b| b == 0)
    {
        return Err(AppError::invalid(
            "JSON must be UTF-8; this file looks like UTF-16 or binary — convert it to UTF-8 first",
        ));
    }
    let body: &[u8] = if head.starts_with(&UTF8_BOM) {
        &head[3..]
    } else {
        &head
    };
    let Some(at) = body.iter().position(|b| !b.is_ascii_whitespace()) else {
        return Err(AppError::invalid("the file contains only whitespace"));
    };
    let lines_ext = matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("jsonl" | "ndjson")
    );

    let shape = if lines_ext {
        DetectedShape::JsonLines
    } else {
        match body[at] {
            b'[' => match body[at + 1..].iter().find(|b| !b.is_ascii_whitespace()) {
                Some(b'{') => DetectedShape::ObjectArray,
                Some(b'[') => DetectedShape::ArrayOfArrays,
                _ => DetectedShape::PrimitiveArray,
            },
            b'{' => detect_brace_shape(&body[at..]),
            _ => DetectedShape::ScalarDocument,
        }
    };

    let mut candidates = Vec::new();
    let mut note = None;
    if shape == DetectedShape::ObjectDocument && want_candidates {
        if file_len > DOC_PARSE_LIMIT {
            note = Some(format!(
                "the file is larger than {} MiB, so record-array candidates were not \
                 auto-detected; enter a JSON Pointer to the record array",
                DOC_PARSE_LIMIT / (1024 * 1024)
            ));
        } else {
            let bytes = std::fs::read(path)?;
            let (doc_bytes, base) = if bytes.starts_with(&UTF8_BOM) {
                (&bytes[3..], 3u64)
            } else {
                (&bytes[..], 0u64)
            };
            let doc: JVal = serde_json::from_slice(doc_bytes)
                .map_err(|e| located_invalid(doc_bytes, &e, base, 1))?;
            candidates = collect_candidates(&doc);
        }
    }
    Ok(ShapeInfo {
        shape,
        candidates,
        note,
    })
}

fn detect_brace_shape(rest: &[u8]) -> DetectedShape {
    match rest.iter().position(|&b| b == b'\n') {
        None => DetectedShape::ObjectDocument,
        Some(nl) => {
            let mut first_line = &rest[..nl];
            if first_line.ends_with(b"\r") {
                first_line = &first_line[..first_line.len() - 1];
            }
            let more = rest[nl + 1..].iter().any(|b| !b.is_ascii_whitespace());
            if more && serde_json::from_slice::<IgnoredAny>(first_line).is_ok() {
                DetectedShape::JsonLines
            } else {
                DetectedShape::ObjectDocument
            }
        }
    }
}

fn element_kind(items: &[JVal]) -> &'static str {
    if items.is_empty() {
        return "empty";
    }
    if items.iter().all(|v| matches!(v, JVal::Obj(_))) {
        "object"
    } else if items.iter().all(|v| matches!(v, JVal::Arr(_))) {
        "array"
    } else if items.iter().all(is_primitive) {
        "primitive"
    } else {
        "mixed"
    }
}

fn escape_pointer_segment(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn collect_candidates(doc: &JVal) -> Vec<PointerCandidate> {
    type Found = (u8, u64, usize, String, &'static str);
    fn walk(v: &JVal, pointer: &str, depth: usize, found: &mut Vec<Found>) {
        if depth > CANDIDATE_DEPTH {
            return;
        }
        match v {
            JVal::Arr(items) => {
                let kind = element_kind(items);
                let rank = match kind {
                    "object" => 0,
                    "array" => 1,
                    "primitive" => 2,
                    "mixed" => 3,
                    _ => 4,
                };
                found.push((rank, items.len() as u64, depth, pointer.to_string(), kind));
                // Recurse into a few leading elements only: homogeneous
                // record arrays would otherwise spam per-element candidates.
                for (i, item) in items.iter().take(3).enumerate() {
                    walk(item, &format!("{pointer}/{i}"), depth + 1, found);
                }
            }
            JVal::Obj(entries) => {
                for (k, val) in entries {
                    let seg = escape_pointer_segment(k);
                    walk(val, &format!("{pointer}/{seg}"), depth + 1, found);
                }
            }
            _ => {}
        }
    }
    let mut found: Vec<Found> = Vec::new();
    walk(doc, "", 0, &mut found);
    found.sort_by(|a, b| (a.0, Reverse(a.1), a.2, &a.3).cmp(&(b.0, Reverse(b.1), b.2, &b.3)));
    found.truncate(MAX_CANDIDATES);
    found
        .into_iter()
        .map(|(_, records, _, pointer, kind)| PointerCandidate {
            pointer,
            records,
            element_kind: kind.to_string(),
        })
        .collect()
}

// ----- JSON Pointer ----------------------------------------------------------------

/// Parse an RFC 6901 JSON Pointer into unescaped segments (`""` → root).
fn parse_pointer(pointer: &str) -> AppResult<Vec<String>> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    let Some(rest) = pointer.strip_prefix('/') else {
        return Err(AppError::invalid(
            "a JSON Pointer must start with '/' (or be empty for the document root)",
        ));
    };
    rest.split('/').map(unescape_pointer_segment).collect()
}

fn unescape_pointer_segment(seg: &str) -> AppResult<String> {
    let mut out = String::with_capacity(seg.len());
    let mut chars = seg.chars();
    while let Some(c) = chars.next() {
        if c == '~' {
            match chars.next() {
                Some('0') => out.push('~'),
                Some('1') => out.push('/'),
                other => {
                    return Err(AppError::invalid(format!(
                        "invalid JSON Pointer escape '~{}'",
                        other.map(String::from).unwrap_or_default()
                    )))
                }
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

// ----- streaming pointer resolution ------------------------------------------------

/// Sink shared by the streaming walk: receives each record of the target
/// array; failures are stashed so the real [`AppError`] survives the trip
/// through serde's error type.
struct SinkState<'a> {
    f: &'a mut dyn FnMut(u64, JVal) -> AppResult<()>,
    count: u64,
    failure: Option<AppError>,
}

/// `DeserializeSeed` that walks the remaining pointer segments without
/// materialising anything outside the target array, then streams the target
/// array's elements one at a time into the sink.
struct PointerSeed<'a, 'b> {
    rest: &'b [String],
    state: &'b mut SinkState<'a>,
}

impl<'de> DeserializeSeed<'de> for PointerSeed<'_, '_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for PointerSeed<'_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an array of records at the JSON Pointer")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        if self.rest.is_empty() {
            // The target: stream the elements.
            let state = self.state;
            let mut index = 0u64;
            while seq
                .next_element_seed(ElementSeed {
                    state: &mut *state,
                    index,
                })?
                .is_some()
            {
                index += 1;
            }
            state.count = index;
            return Ok(());
        }
        let seg = &self.rest[0];
        let want: u64 = seg.parse().map_err(|_| {
            A::Error::custom(format!(
                "JSON Pointer segment '{seg}' is not an array index, but the value here is an array"
            ))
        })?;
        let mut state = Some(self.state);
        let mut matched = false;
        let mut i = 0u64;
        loop {
            if i == want && !matched {
                let st = state.take().expect("index matched once");
                match seq.next_element_seed(PointerSeed {
                    rest: &self.rest[1..],
                    state: st,
                })? {
                    Some(()) => matched = true,
                    None => break,
                }
            } else if seq.next_element::<IgnoredAny>()?.is_none() {
                break;
            }
            i += 1;
        }
        if matched {
            Ok(())
        } else {
            Err(A::Error::custom(format!(
                "JSON Pointer index {want} is out of range (the array has {i} elements)"
            )))
        }
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let Some(seg) = self.rest.first() else {
            return Err(A::Error::custom(
                "the JSON Pointer target is an object, not an array of records",
            ));
        };
        let mut state = Some(self.state);
        let mut matched = false;
        while let Some(key) = map.next_key::<String>()? {
            if !matched && key == *seg {
                let st = state.take().expect("key matched once");
                map.next_value_seed(PointerSeed {
                    rest: &self.rest[1..],
                    state: st,
                })?;
                matched = true;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        if matched {
            Ok(())
        } else {
            Err(A::Error::custom(format!(
                "JSON Pointer segment '{seg}' was not found"
            )))
        }
    }
}

/// Deserializes ONE record array element into a [`JVal`] and hands it to
/// the sink immediately (bounded memory: one element at a time).
struct ElementSeed<'a, 'b> {
    state: &'b mut SinkState<'a>,
    index: u64,
}

impl<'de> DeserializeSeed<'de> for ElementSeed<'_, '_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        let value = JVal::deserialize(deserializer)?;
        match (self.state.f)(self.index, value) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.state.failure = Some(e);
                Err(D::Error::custom("import stopped"))
            }
        }
    }
}

/// `Read` wrapper that streams progress (bytes) into the job context and
/// observes cancellation between reads.
struct CountingReader<'a> {
    inner: File,
    ctx: Option<&'a JobCtx>,
}

impl Read for CountingReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(ctx) = self.ctx {
            if ctx.is_cancelled() {
                return Err(std::io::Error::other("cancelled"));
            }
        }
        let n = self.inner.read(buf)?;
        if let Some(ctx) = self.ctx {
            if ctx.advance(n as u64).is_err() {
                return Err(std::io::Error::other("cancelled"));
            }
        }
        Ok(n)
    }
}

/// Stream the record array at `segments` inside the JSON document at
/// `path`, feeding each element to `f`. Returns the record count.
/// Advance `file` past a leading UTF-8 BOM if present (leaving it at byte 0
/// otherwise). `serde_json` does NOT tolerate a BOM, so every raw-reader parse
/// must skip it first — `detect_shape` and the JSONL framer already do.
fn skip_utf8_bom(file: &mut File) -> std::io::Result<()> {
    let mut probe = [0u8; 3];
    let mut got = 0usize;
    while got < 3 {
        let n = file.read(&mut probe[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    if !(got == 3 && probe == UTF8_BOM) {
        file.seek(SeekFrom::Start(0))?;
    }
    Ok(())
}

fn stream_doc(
    path: &Path,
    segments: &[String],
    ctx: Option<&JobCtx>,
    f: &mut dyn FnMut(u64, Loc, JVal) -> AppResult<()>,
) -> AppResult<u64> {
    let mut file = File::open(path)?;
    // A BOM-prefixed array/object document detects fine (detect_shape strips
    // it) but serde_json would choke on it; skip it before the streaming parse.
    skip_utf8_bom(&mut file)?;
    let reader = BufReader::with_capacity(64 * 1024, CountingReader { inner: file, ctx });
    let mut de = serde_json::Deserializer::from_reader(reader);
    let mut adapter = |index: u64, value: JVal| {
        f(
            index,
            Loc {
                record: index,
                line: None,
            },
            value,
        )
    };
    let mut state = SinkState {
        f: &mut adapter,
        count: 0,
        failure: None,
    };
    let result = PointerSeed {
        rest: segments,
        state: &mut state,
    }
    .deserialize(&mut de)
    .and_then(|()| de.end());
    match result {
        Ok(()) => Ok(state.count),
        Err(e) => {
            if let Some(app) = state.failure.take() {
                return Err(app);
            }
            if ctx.is_some_and(|c| c.is_cancelled()) {
                return Err(AppError::Cancelled);
            }
            Err(located_invalid_in_file(path, &e))
        }
    }
}

// ----- JSON Lines framing ----------------------------------------------------------

/// Byte-offset record index over a JSON Lines file: one `u64` line-start
/// offset per (non-blank) record, mirroring the F10 CSV record index —
/// windowed reads seek to `offsets[start]` and parse only the requested
/// records. Built as a side effect of the (bounded-memory) scan pass.
#[derive(Debug, Clone)]
pub struct JsonlIndex {
    /// Absolute byte offset of each record's line start, in order.
    pub offsets: Vec<u64>,
    /// Total data length (end offset of the last record's line).
    pub data_len: u64,
}

impl JsonlIndex {
    pub fn n_records(&self) -> usize {
        self.offsets.len()
    }

    /// Read records `[start, end)` as raw line bytes (seek + one contiguous
    /// read, like [`crate::index::IndexHandle`] windows).
    pub fn read_records(&self, path: &Path, start: usize, end: usize) -> AppResult<Vec<Vec<u8>>> {
        let end = end.min(self.offsets.len());
        if start >= end {
            return Ok(Vec::new());
        }
        let a = self.offsets[start];
        let b = if end == self.offsets.len() {
            self.data_len
        } else {
            self.offsets[end]
        };
        let mut file = File::open(path)?;
        if file.metadata()?.len() < b {
            return Err(AppError::Other(
                "the source file changed on disk; reload the document".into(),
            ));
        }
        file.seek(SeekFrom::Start(a))?;
        let mut window = vec![0u8; (b - a) as usize];
        file.read_exact(&mut window)?;
        let mut out = Vec::with_capacity(end - start);
        for mut line in window.split(|&b| b == b'\n') {
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if line.iter().all(|c| c.is_ascii_whitespace()) {
                continue;
            }
            out.push(line.to_vec());
        }
        if out.len() != end - start {
            return Err(AppError::Other(
                "the source file changed on disk; reload the document".into(),
            ));
        }
        Ok(out)
    }
}

/// Line sink for the JSONL framer:
/// `(record_index, byte_offset, physical_line_no, line_bytes)`.
type LineSink<'a> = dyn FnMut(u64, u64, u64, &[u8]) -> AppResult<()> + 'a;

/// Stream `path` line by line with bounded memory: fixed-size chunks are
/// read into a carry buffer, complete lines are emitted as they appear,
/// blank lines are skipped, CRLF and a UTF-8 BOM are tolerated. Returns
/// `(data_len, peak_buffer)` — the peak carry-buffer size is at most the
/// longest line plus one chunk, never the whole file (tests assert it).
fn stream_jsonl(
    path: &Path,
    chunk_size: usize,
    ctx: Option<&JobCtx>,
    f: &mut LineSink<'_>,
) -> AppResult<(u64, usize)> {
    fn emit(
        idx: &mut u64,
        line: &[u8],
        offset: u64,
        line_no: u64,
        f: &mut LineSink<'_>,
    ) -> AppResult<()> {
        let line = if line.last() == Some(&b'\r') {
            &line[..line.len() - 1]
        } else {
            line
        };
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(());
        }
        f(*idx, offset, line_no, line)?;
        *idx += 1;
        Ok(())
    }

    let mut file = File::open(path)?;
    // BOM probe: read up to 3 bytes; a non-BOM prefix seeds the buffer.
    let mut bom = [0u8; 3];
    let mut got = 0usize;
    while got < 3 {
        let n = file.read(&mut bom[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    let (mut pos, mut buf): (u64, Vec<u8>) = if got == 3 && bom == UTF8_BOM {
        (3, Vec::new())
    } else {
        (0, bom[..got].to_vec())
    };

    let mut chunk = vec![0u8; chunk_size.max(1)];
    let mut line_no: u64 = 1;
    let mut idx: u64 = 0;
    let mut peak = buf.len();
    let mut first_pass = true;
    let mut eof = false;
    while !eof {
        if !first_pass {
            let n = file.read(&mut chunk)?;
            if n == 0 {
                eof = true;
            } else {
                buf.extend_from_slice(&chunk[..n]);
                peak = peak.max(buf.len());
                if let Some(ctx) = ctx {
                    ctx.advance(n as u64)?;
                }
            }
        }
        first_pass = false;
        let mut start = 0usize;
        while let Some(rel) = buf[start..].iter().position(|&b| b == b'\n') {
            let end = start + rel;
            emit(&mut idx, &buf[start..end], pos + start as u64, line_no, f)?;
            line_no += 1;
            start = end + 1;
        }
        buf.drain(..start);
        pos += start as u64;
    }
    if !buf.is_empty() {
        emit(&mut idx, &buf, pos, line_no, f)?;
        pos += buf.len() as u64;
    }
    Ok((pos, peak))
}

// ----- error locations -------------------------------------------------------------

/// serde_json's Display appends " at line L column C"; our messages carry
/// their own (absolute) position, so strip the relative one.
fn clean_msg(e: &serde_json::Error) -> String {
    let s = e.to_string();
    s.split(" at line ").next().unwrap_or(&s).to_string()
}

fn printable(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn format_json_error(offset: u64, line: u64, col: u64, msg: &str, near: &str) -> AppError {
    AppError::invalid(format!(
        "invalid JSON at byte offset {offset} (line {line}, column {col}): {msg}; near \"{near}\""
    ))
}

/// 0-based byte index of 1-based (line, column) within `bytes`.
fn position_in_bytes(bytes: &[u8], line: u64, col: u64) -> usize {
    let mut idx = 0usize;
    let mut remaining = line.saturating_sub(1);
    while remaining > 0 && idx < bytes.len() {
        if bytes[idx] == b'\n' {
            remaining -= 1;
        }
        idx += 1;
    }
    (idx + col.saturating_sub(1) as usize).min(bytes.len())
}

/// Locate a serde_json error inside an in-memory slice whose first byte
/// sits at absolute `base_offset` / physical line `base_line`.
fn located_invalid(
    bytes: &[u8],
    e: &serde_json::Error,
    base_offset: u64,
    base_line: u64,
) -> AppError {
    let msg = clean_msg(e);
    let (line, col) = (e.line() as u64, e.column() as u64);
    if line == 0 {
        return AppError::invalid(msg);
    }
    let local = position_in_bytes(bytes, line, col);
    let start = local.saturating_sub(CONTEXT_RADIUS);
    let end = (local + CONTEXT_RADIUS).min(bytes.len());
    format_json_error(
        base_offset + local as u64,
        base_line + line - 1,
        col,
        &msg,
        &printable(&bytes[start..end]),
    )
}

/// Locate a serde_json error raised while STREAMING a file: rescan the file
/// (error path only) to turn (line, column) into a byte offset + context.
fn located_invalid_in_file(path: &Path, e: &serde_json::Error) -> AppError {
    let msg = clean_msg(e);
    let (line, col) = (e.line() as u64, e.column() as u64);
    if line == 0 {
        return AppError::invalid(msg);
    }
    match offset_in_file(path, line, col) {
        Some((offset, near)) => format_json_error(offset, line, col, &msg, &near),
        None => AppError::invalid(format!("invalid JSON (line {line}, column {col}): {msg}")),
    }
}

fn offset_in_file(path: &Path, line: u64, col: u64) -> Option<(u64, String)> {
    let mut file = File::open(path).ok()?;
    let mut remaining = line - 1;
    let mut pos: u64 = 0;
    let mut chunk = vec![0u8; 64 * 1024];
    'outer: while remaining > 0 {
        let n = file.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        for (i, &b) in chunk[..n].iter().enumerate() {
            if b == b'\n' {
                remaining -= 1;
                if remaining == 0 {
                    pos += i as u64 + 1;
                    continue 'outer;
                }
            }
        }
        pos += n as u64;
    }
    let offset = pos + col.saturating_sub(1);
    let start = offset.saturating_sub(CONTEXT_RADIUS as u64);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut window = [0u8; CONTEXT_RADIUS * 2];
    let mut len = 0usize;
    while len < window.len() {
        let n = file.read(&mut window[len..]).ok()?;
        if n == 0 {
            break;
        }
        len += n;
    }
    Some((offset, printable(&window[..len])))
}

// ----- record streaming ------------------------------------------------------------

/// Where records come from once the shape is resolved.
#[derive(Debug, Clone)]
enum InputMode {
    /// JSON Lines: one record per line.
    Lines,
    /// A record array at the given (already parsed) pointer segments.
    Doc(Vec<String>),
}

/// A record's position, for error messages ("record 3 (line 4)").
#[derive(Debug, Clone, Copy)]
struct Loc {
    record: u64,
    line: Option<u64>,
}

impl fmt::Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "record {} (line {line})", self.record + 1),
            None => write!(f, "record {}", self.record + 1),
        }
    }
}

/// Run one full streaming pass over the records, in order. `offsets`, when
/// given, collects the JSONL byte-offset index along the way.
fn stream_records(
    path: &Path,
    mode: &InputMode,
    ctx: Option<&JobCtx>,
    mut offsets: Option<&mut Vec<u64>>,
    f: &mut dyn FnMut(u64, Loc, JVal) -> AppResult<()>,
) -> AppResult<u64> {
    match mode {
        InputMode::Lines => {
            let mut count = 0u64;
            stream_jsonl(
                path,
                JSONL_CHUNK,
                ctx,
                &mut |idx, offset, line_no, bytes| {
                    if let Some(offs) = offsets.as_deref_mut() {
                        offs.push(offset);
                    }
                    let value: JVal = serde_json::from_slice(bytes)
                        .map_err(|e| located_invalid(bytes, &e, offset, line_no))?;
                    count = idx + 1;
                    f(
                        idx,
                        Loc {
                            record: idx,
                            line: Some(line_no),
                        },
                        value,
                    )
                },
            )?;
            Ok(count)
        }
        InputMode::Doc(segments) => stream_doc(path, segments, ctx, f),
    }
}

// ----- flattening and explosion ----------------------------------------------------

/// How records present themselves (fixed by the first record).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordKind {
    /// JSON objects: keys become columns.
    Object,
    /// JSON arrays: positions become `Column N` columns.
    Row,
    /// Primitives: a single `value` column.
    Value,
}

fn kind_of(v: &JVal) -> RecordKind {
    match v {
        JVal::Obj(_) => RecordKind::Object,
        JVal::Arr(_) => RecordKind::Row,
        _ => RecordKind::Value,
    }
}

fn kind_name(kind: RecordKind) -> &'static str {
    match kind {
        RecordKind::Object => "object",
        RecordKind::Row => "array",
        RecordKind::Value => "value",
    }
}

/// One produced cell before token narrowing: explicit null stays a distinct
/// state, text carries the JSON kind it came from (for type inference).
#[derive(Debug, Clone)]
enum CellVal {
    Null,
    Text(String, TextKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextKind {
    Str,
    Int,
    Float,
    Bool,
    Json,
}

/// One record after the policy walk: scalar cells by path, explode
/// dimensions, and structure notes for the preview.
#[derive(Debug, Clone, Default)]
struct FlatRecord {
    cells: Vec<(String, CellVal)>,
    by_path: HashMap<String, usize>,
    dims: Vec<(String, Vec<JVal>)>,
    nested_paths: Vec<String>,
    /// `(path, len, primitives_only)` for every array encountered.
    arrays: Vec<(String, usize, bool)>,
}

impl FlatRecord {
    /// Insert a cell; a duplicate path replaces the earlier value
    /// (duplicate-key last-wins).
    fn put(&mut self, path: String, cell: CellVal) {
        match self.by_path.entry(path) {
            Entry::Occupied(e) => {
                let i = *e.get();
                self.cells[i].1 = cell;
            }
            Entry::Vacant(e) => {
                let i = self.cells.len();
                self.cells.push((e.key().clone(), cell));
                e.insert(i);
            }
        }
    }

    fn note_nested(&mut self, path: &str) {
        if !path.is_empty()
            && self.nested_paths.len() < REPORT_LIMIT
            && !self.nested_paths.iter().any(|p| p == path)
        {
            self.nested_paths.push(path.to_string());
        }
    }

    fn base_clone(&self) -> FlatRecord {
        FlatRecord {
            cells: self.cells.clone(),
            by_path: self.by_path.clone(),
            ..FlatRecord::default()
        }
    }
}

fn scalar_cell(v: &JVal) -> Option<CellVal> {
    match v {
        JVal::Null => Some(CellVal::Null),
        JVal::Bool(b) => Some(CellVal::Text(b.to_string(), TextKind::Bool)),
        JVal::Num(n) => {
            let kind = if n.is_i64() || n.is_u64() {
                TextKind::Int
            } else {
                TextKind::Float
            };
            Some(CellVal::Text(n.to_string(), kind))
        }
        JVal::Str(s) => Some(CellVal::Text(s.clone(), TextKind::Str)),
        JVal::Obj(_) | JVal::Arr(_) => None,
    }
}

/// The policy walk: turn `v` at `path` into cells (and explode dimensions)
/// on `rec`. `in_element` is true while walking an exploded array element,
/// where further explosion is not allowed.
fn flatten_into(
    path: String,
    v: &JVal,
    res: &Resolved,
    rec: &mut FlatRecord,
    in_element: bool,
    loc: &Loc,
) -> AppResult<()> {
    if res.ignore.contains(path.as_str()) {
        return Ok(());
    }
    if let Some(cell) = scalar_cell(v) {
        rec.put(path, cell);
        return Ok(());
    }
    match v {
        JVal::Obj(entries) => {
            rec.note_nested(&path);
            match res.nested {
                NestedPolicy::Flatten => {
                    for (key, value) in entries {
                        flatten_into(join_path(&path, key), value, res, rec, in_element, loc)?;
                    }
                }
                NestedPolicy::PreserveJson => {
                    rec.put(path, CellVal::Text(compact(v), TextKind::Json));
                }
            }
        }
        JVal::Arr(items) => {
            if rec.arrays.len() < REPORT_LIMIT {
                rec.arrays
                    .push((path.clone(), items.len(), items.iter().all(is_primitive)));
            }
            match res.array {
                ArrayPolicy::PreserveJson => {
                    rec.put(path, CellVal::Text(compact(v), TextKind::Json));
                }
                ArrayPolicy::Reject => {
                    return Err(AppError::invalid(format!(
                        "{loc}: '{path}' is an array and the array policy is 'reject'"
                    )));
                }
                ArrayPolicy::Join => {
                    let mut parts = Vec::with_capacity(items.len());
                    for item in items {
                        match item {
                            JVal::Null => parts.push(res.null_token.clone()),
                            JVal::Bool(b) => parts.push(b.to_string()),
                            JVal::Num(n) => parts.push(n.to_string()),
                            JVal::Str(s) => parts.push(s.clone()),
                            JVal::Obj(_) | JVal::Arr(_) => {
                                return Err(AppError::invalid(format!(
                                    "{loc}: the array at '{path}' contains nested \
                                     objects/arrays and cannot be joined; use the \
                                     preserveJson policy for it instead"
                                )));
                            }
                        }
                    }
                    rec.put(
                        path,
                        CellVal::Text(parts.join(&res.join_sep), TextKind::Str),
                    );
                }
                ArrayPolicy::Explode => {
                    if in_element {
                        return Err(AppError::invalid(format!(
                            "{loc}: '{path}' is an array inside an exploded array element; \
                             nested explode is not supported — use preserveJson or join \
                             for inner arrays"
                        )));
                    }
                    if let Some(dim) = rec.dims.iter_mut().find(|(p, _)| *p == path) {
                        dim.1 = items.clone(); // duplicate key: last wins
                    } else {
                        rec.dims.push((path, items.clone()));
                    }
                }
            }
        }
        _ => unreachable!("scalars handled above"),
    }
    Ok(())
}

/// Walk one whole record according to its kind.
fn flatten_record(kind: RecordKind, v: &JVal, res: &Resolved, loc: &Loc) -> AppResult<FlatRecord> {
    let actual = kind_of(v);
    if actual != kind {
        return Err(AppError::invalid(format!(
            "{loc}: this record is a JSON {}, but earlier records are {}s; records must \
             share one shape",
            kind_name(actual),
            kind_name(kind)
        )));
    }
    let mut rec = FlatRecord::default();
    match v {
        JVal::Obj(entries) => {
            for (key, value) in entries {
                flatten_into(escape_key(key), value, res, &mut rec, false, loc)?;
            }
        }
        JVal::Arr(items) => {
            for (i, value) in items.iter().enumerate() {
                flatten_into(
                    format!("Column {}", i + 1),
                    value,
                    res,
                    &mut rec,
                    false,
                    loc,
                )?;
            }
        }
        other => flatten_into("value".to_string(), other, res, &mut rec, false, loc)?,
    }
    Ok(rec)
}

fn multi_choice_error(rec: &FlatRecord, loc: &Loc) -> AppError {
    let paths: Vec<&str> = rec.dims.iter().map(|(p, _)| p.as_str()).collect();
    AppError::invalid(format!(
        "{loc}: {} array fields would explode ({}); choose an explicit cartesian or zip \
         combination (multiArray)",
        rec.dims.len(),
        paths.join(", ")
    ))
}

/// Number of output rows a record's dimensions produce (matches
/// [`expand_record`] exactly).
fn combo_count(dims: &[(String, Vec<JVal>)], multi: Option<MultiArrayMode>) -> u64 {
    match dims.len() {
        0 => 1,
        1 => dims[0].1.len().max(1) as u64,
        _ => match multi {
            Some(MultiArrayMode::Zip) => {
                dims.iter().map(|(_, i)| i.len()).max().unwrap_or(1).max(1) as u64
            }
            // Cartesian. An unresolved (None) choice is projected as the
            // cartesian upper bound for the preview; the import pass rejects it.
            _ => dims.iter().fold(1u64, |acc, (_, i)| {
                acc.saturating_mul(i.len().max(1) as u64)
            }),
        },
    }
}

/// Expand a record into its output rows. Rows are produced one at a time
/// (bounded memory even for large cartesian products); the callback returns
/// `Ok(false)` to stop early. An empty (or exhausted, under zip) dimension
/// contributes nothing to the row — the field stays missing.
fn expand_record(
    rec: &FlatRecord,
    res: &Resolved,
    loc: &Loc,
    out: &mut dyn FnMut(&FlatRecord) -> AppResult<bool>,
) -> AppResult<()> {
    if rec.dims.is_empty() {
        out(rec)?;
        return Ok(());
    }
    let slots: Vec<usize> = rec
        .dims
        .iter()
        .map(|(_, items)| items.len().max(1))
        .collect();
    let base = rec.base_clone();
    if rec.dims.len() > 1 && res.multi == Some(MultiArrayMode::Zip) {
        let n = slots.iter().copied().max().unwrap_or(1);
        for i in 0..n {
            let mut row = base.base_clone();
            for (path, items) in &rec.dims {
                if i < items.len() {
                    flatten_into(path.clone(), &items[i], res, &mut row, true, loc)?;
                }
            }
            if !out(&row)? {
                return Ok(());
            }
        }
        return Ok(());
    }
    // Cartesian (also the single-dimension case): odometer with the LAST
    // dimension varying fastest.
    let mut idxs = vec![0usize; slots.len()];
    loop {
        let mut row = base.base_clone();
        for (d, (path, items)) in rec.dims.iter().enumerate() {
            if idxs[d] < items.len() {
                flatten_into(path.clone(), &items[idxs[d]], res, &mut row, true, loc)?;
            }
        }
        if !out(&row)? {
            return Ok(());
        }
        let mut d = slots.len();
        loop {
            if d == 0 {
                return Ok(());
            }
            d -= 1;
            idxs[d] += 1;
            if idxs[d] < slots[d] {
                break;
            }
            idxs[d] = 0;
        }
    }
}

// ----- the scan pass ---------------------------------------------------------------

#[derive(Debug, Default)]
struct ColStat {
    present: u64,
    nulls: u64,
    saw_str: bool,
    saw_int: bool,
    saw_float: bool,
    saw_bool: bool,
    saw_json: bool,
}

impl ColStat {
    fn note(&mut self, kind: TextKind) {
        match kind {
            TextKind::Str => self.saw_str = true,
            TextKind::Int => self.saw_int = true,
            TextKind::Float => self.saw_float = true,
            TextKind::Bool => self.saw_bool = true,
            TextKind::Json => self.saw_json = true,
        }
    }

    /// Native-JSON type inference: homogeneous columns get their JSON type,
    /// anything mixed (or string-bearing) stays text.
    fn inferred(&self) -> LogicalType {
        if self.saw_str {
            return LogicalType::Text;
        }
        let numeric = self.saw_int || self.saw_float;
        match (numeric, self.saw_bool, self.saw_json) {
            (true, false, false) => {
                if self.saw_float {
                    LogicalType::Float
                } else {
                    LogicalType::Integer
                }
            }
            (false, true, false) => LogicalType::Boolean,
            (false, false, true) => LogicalType::Json,
            _ => LogicalType::Text,
        }
    }
}

#[derive(Debug)]
struct ArrayAgg {
    occurrences: u64,
    max_len: usize,
    primitives_only: bool,
}

#[derive(Debug, Default)]
struct ScanState {
    kind: Option<RecordKind>,
    columns: Vec<String>,
    index_of: HashMap<String, usize>,
    stats: Vec<ColStat>,
    nested: BTreeSet<String>,
    arrays: BTreeMap<String, ArrayAgg>,
    records: u64,
    projected_rows: u64,
    samples: Vec<Vec<(String, Option<String>)>>,
    null_collisions: u64,
    missing_collisions: u64,
    exploded: bool,
    /// The most array dimensions any SINGLE record explodes along at once.
    /// `>= 2` is exactly the backend's per-record condition for demanding an
    /// explicit `multiArray` (cartesian/zip) choice, so the preview can mirror
    /// it instead of over-approximating from the document-wide array list.
    max_record_dims: usize,
}

impl ScanState {
    fn scan_record(&mut self, res: &Resolved, loc: &Loc, value: &JVal) -> AppResult<()> {
        let kind = kind_of(value);
        match self.kind {
            None => self.kind = Some(kind),
            Some(k) if k != kind => {
                return Err(AppError::invalid(format!(
                    "{loc}: this record is a JSON {}, but earlier records are {}s; records \
                     must share one shape",
                    kind_name(kind),
                    kind_name(k)
                )))
            }
            Some(_) => {}
        }
        let rec = flatten_record(kind, value, res, loc)?;
        // Record — but do NOT reject here — records that explode along two or
        // more array dimensions at once. The preview stays permissive (rows
        // projected under the cartesian upper bound) so the dialog can prompt
        // for the combine mode inline and can tell a genuine co-occurrence
        // apart from two array fields that merely both exist somewhere in a
        // heterogeneous file. The import/emit pass (see `import`) is the
        // authority that rejects a missing multiArray choice, so "no partial
        // documents on invalid input" still holds.
        self.max_record_dims = self.max_record_dims.max(rec.dims.len());
        let first_record = self.records == 0;
        self.records += 1;
        self.projected_rows = self
            .projected_rows
            .saturating_add(combo_count(&rec.dims, res.multi));
        if !rec.dims.is_empty() {
            self.exploded = true;
        }

        // Occurrences: record-level cells first, then every explode
        // element's cells (element structure contributes columns too), in
        // walk order. Element-derived columns therefore count per element.
        let mut occ: Vec<(String, CellVal)> = rec.cells.clone();
        let mut nested: Vec<String> = rec.nested_paths.clone();
        let mut arrays: Vec<(String, usize, bool)> = rec.arrays.clone();
        for (path, items) in &rec.dims {
            for item in items {
                let mut sub = FlatRecord::default();
                flatten_into(path.clone(), item, res, &mut sub, true, loc)?;
                occ.extend(sub.cells);
                nested.extend(sub.nested_paths);
                arrays.extend(sub.arrays);
            }
        }

        // Deterministic key union (module docs): the first record's new
        // paths append in document order, later records' new paths append
        // alphabetically.
        let mut new_paths: Vec<&str> = Vec::new();
        for (path, _) in &occ {
            if !self.index_of.contains_key(path.as_str()) && !new_paths.contains(&path.as_str()) {
                new_paths.push(path);
            }
        }
        if !first_record {
            new_paths.sort_unstable();
        }
        for path in new_paths {
            let i = self.columns.len();
            self.index_of.insert(path.to_string(), i);
            self.columns.push(path.to_string());
            self.stats.push(ColStat::default());
        }
        if self.columns.len() > MAX_COLUMNS {
            return Err(AppError::invalid(format!(
                "the import would create more than {MAX_COLUMNS} columns; preserve deep \
                 structures as JSON instead of flattening them"
            )));
        }

        for (path, cell) in &occ {
            let i = self.index_of[path.as_str()];
            let stat = &mut self.stats[i];
            match cell {
                CellVal::Null => stat.nulls += 1,
                CellVal::Text(text, kind) => {
                    stat.present += 1;
                    stat.note(*kind);
                    if *kind == TextKind::Str {
                        if *text == res.null_token {
                            self.null_collisions += 1;
                        } else if *text == res.missing_token {
                            self.missing_collisions += 1;
                        }
                    }
                }
            }
        }
        for path in nested {
            if self.nested.len() < REPORT_LIMIT {
                self.nested.insert(path);
            }
        }
        for (path, len, primitives) in arrays {
            if self.arrays.len() >= REPORT_LIMIT && !self.arrays.contains_key(&path) {
                continue;
            }
            let agg = self.arrays.entry(path).or_insert(ArrayAgg {
                occurrences: 0,
                max_len: 0,
                primitives_only: true,
            });
            agg.occurrences += 1;
            agg.max_len = agg.max_len.max(len);
            agg.primitives_only &= primitives;
        }
        if self.samples.len() < SAMPLE_ROWS {
            let samples = &mut self.samples;
            expand_record(&rec, res, loc, &mut |row| {
                samples.push(
                    row.cells
                        .iter()
                        .map(|(path, cell)| {
                            let text = match cell {
                                CellVal::Null => None,
                                CellVal::Text(t, _) => Some(t.clone()),
                            };
                            (path.clone(), text)
                        })
                        .collect(),
                );
                Ok(samples.len() < SAMPLE_ROWS)
            })?;
        }
        Ok(())
    }
}

// ----- preview DTOs ----------------------------------------------------------------

/// One column of the import preview.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewColumn {
    /// Flattened path name (escaping rule in the module docs).
    pub name: String,
    pub inferred_type: LogicalType,
    /// Occurrences with a value (per record; per element for columns that
    /// come from exploded array elements).
    pub present: u64,
    /// Occurrences that were explicit JSON null.
    pub nulls: u64,
    /// Records in which the path did not occur at all.
    pub missing: u64,
}

/// One array-valued field, for the preview's policy picker.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrayFieldInfo {
    pub path: String,
    pub occurrences: u64,
    pub max_len: usize,
    pub primitives_only: bool,
}

/// Everything the import dialog needs to render (bounded samples; exact
/// counts from a full scan).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonImportPreview {
    pub shape: DetectedShape,
    /// The pointer actually used (empty string = root), when resolved.
    pub pointer: Option<String>,
    /// True when an object document needs a record-array pointer chosen
    /// before anything can be scanned.
    pub needs_pointer: bool,
    pub candidates: Vec<PointerCandidate>,
    /// `"object"`, `"array"` or `"value"`, when known.
    pub record_kind: Option<String>,
    pub columns: Vec<PreviewColumn>,
    pub nested_object_paths: Vec<String>,
    pub array_fields: Vec<ArrayFieldInfo>,
    pub record_count: u64,
    /// Rows the import will produce (explosion accounted for).
    pub projected_rows: u64,
    pub projected_columns: usize,
    /// The most array dimensions any single record explodes along at once
    /// under the current options. `>= 2` means an explicit `multiArray`
    /// (cartesian/zip) choice is required; the frontend gate mirrors this
    /// rather than counting the document-wide array-field list.
    pub max_record_dims: usize,
    /// Up to [`SAMPLE_ROWS`] rows exactly as they would land in the grid
    /// (null/missing tokens applied).
    pub sample_rows: Vec<Vec<String>>,
    pub exploded: bool,
    pub warnings: Vec<String>,
}

/// A completed scan: the public preview plus the private emit plan the
/// import pass replays.
pub struct JsonImportScan {
    pub preview: JsonImportPreview,
    /// The JSONL byte-offset record index (JSON Lines inputs only).
    pub jsonl_index: Option<JsonlIndex>,
    mode: Option<InputMode>,
    kind: Option<RecordKind>,
    columns: Vec<String>,
    inferred: Vec<LogicalType>,
    nulls: Vec<u64>,
    fingerprint: Option<FileFingerprint>,
}

impl JsonImportScan {
    fn needs_pointer(shape: ShapeInfo, fingerprint: Option<FileFingerprint>) -> JsonImportScan {
        let mut warnings = Vec::new();
        if let Some(note) = shape.note {
            warnings.push(note);
        }
        JsonImportScan {
            preview: JsonImportPreview {
                shape: shape.shape,
                pointer: None,
                needs_pointer: true,
                candidates: shape.candidates,
                record_kind: None,
                columns: Vec::new(),
                nested_object_paths: Vec::new(),
                array_fields: Vec::new(),
                record_count: 0,
                projected_rows: 0,
                projected_columns: 0,
                max_record_dims: 0,
                sample_rows: Vec::new(),
                exploded: false,
                warnings,
            },
            jsonl_index: None,
            mode: None,
            kind: None,
            columns: Vec::new(),
            inferred: Vec::new(),
            nulls: Vec::new(),
            fingerprint,
        }
    }
}

/// Scan the whole input (pass 1): validate every record, compute the
/// deterministic key union, per-column stats, structure notes and bounded
/// sample rows. Nothing is written; the result feeds both the preview UI
/// and [`import`].
pub fn scan(
    path: &Path,
    options: &JsonImportOptions,
    ctx: Option<&JobCtx>,
) -> AppResult<JsonImportScan> {
    scan_impl(path, options, ctx, true)
}

fn scan_impl(
    path: &Path,
    options: &JsonImportOptions,
    ctx: Option<&JobCtx>,
    set_total: bool,
) -> AppResult<JsonImportScan> {
    let res = options.resolve()?;
    let fingerprint = util::stat_fingerprint(path);
    let file_len = std::fs::metadata(path)?.len();
    if set_total {
        if let Some(ctx) = ctx {
            ctx.set_total(file_len);
        }
    }
    let shape_info = detect_shape_inner(path, options.pointer.is_none())?;
    let shape = shape_info.shape;
    let pointer_used: Option<String>;
    let mode = match shape {
        DetectedShape::JsonLines => {
            if options.pointer.as_deref().is_some_and(|p| !p.is_empty()) {
                return Err(AppError::invalid(
                    "JSON Pointers do not apply to JSON Lines input; each line is one record",
                ));
            }
            pointer_used = None;
            InputMode::Lines
        }
        DetectedShape::ObjectArray
        | DetectedShape::ArrayOfArrays
        | DetectedShape::PrimitiveArray => {
            let pointer = options.pointer.clone().unwrap_or_default();
            let segments = parse_pointer(&pointer)?;
            pointer_used = Some(pointer);
            InputMode::Doc(segments)
        }
        DetectedShape::ObjectDocument => match &options.pointer {
            Some(pointer) => {
                let segments = parse_pointer(pointer)?;
                pointer_used = Some(pointer.clone());
                InputMode::Doc(segments)
            }
            None => return Ok(JsonImportScan::needs_pointer(shape_info, fingerprint)),
        },
        DetectedShape::ScalarDocument => {
            return Err(AppError::invalid(
                "the file contains a single JSON scalar; there is nothing tabular to import",
            ))
        }
    };

    let mut state = ScanState::default();
    let mut offsets: Vec<u64> = Vec::new();
    let collect_offsets = matches!(mode, InputMode::Lines);
    stream_records(
        path,
        &mode,
        ctx,
        collect_offsets.then_some(&mut offsets),
        &mut |idx, loc, value| {
            if idx.is_multiple_of(CANCEL_EVERY_RECORDS) {
                if let Some(ctx) = ctx {
                    ctx.check()?;
                }
            }
            state.scan_record(&res, &loc, &value)
        },
    )?;
    if state.records == 0 {
        return Err(AppError::invalid(
            "no records found: the record array (or JSON Lines file) is empty",
        ));
    }
    if state.columns.is_empty() {
        return Err(AppError::invalid("the records contain no fields to import"));
    }

    let inferred: Vec<LogicalType> = state.stats.iter().map(ColStat::inferred).collect();
    let nulls: Vec<u64> = state.stats.iter().map(|s| s.nulls).collect();
    let columns: Vec<PreviewColumn> = state
        .columns
        .iter()
        .zip(&state.stats)
        .zip(&inferred)
        .map(|((name, stat), &inferred_type)| PreviewColumn {
            name: name.clone(),
            inferred_type,
            present: stat.present,
            nulls: stat.nulls,
            missing: state.records.saturating_sub(stat.present + stat.nulls),
        })
        .collect();
    let n_cols = state.columns.len();
    let sample_rows: Vec<Vec<String>> = state
        .samples
        .iter()
        .map(|cells| {
            let mut row = vec![res.missing_token.clone(); n_cols];
            for (path, value) in cells {
                if let Some(&i) = state.index_of.get(path.as_str()) {
                    row[i] = value.clone().unwrap_or_else(|| res.null_token.clone());
                }
            }
            row
        })
        .collect();
    let mut warnings = Vec::new();
    if state.null_collisions > 0 {
        warnings.push(format!(
            "{} string value(s) equal the null token {:?} and will be indistinguishable \
             from explicit nulls; pick a different nullToken if that matters",
            state.null_collisions, res.null_token
        ));
    }
    if state.missing_collisions > 0 {
        warnings.push(format!(
            "{} string value(s) equal the missing-value text {:?} and will be \
             indistinguishable from missing fields; pick a different missingToken if \
             that matters",
            state.missing_collisions, res.missing_token
        ));
    }

    let preview = JsonImportPreview {
        shape,
        pointer: pointer_used,
        needs_pointer: false,
        candidates: shape_info.candidates,
        record_kind: state.kind.map(|k| kind_name(k).to_string()),
        columns,
        nested_object_paths: state.nested.into_iter().collect(),
        array_fields: state
            .arrays
            .into_iter()
            .map(|(path, agg)| ArrayFieldInfo {
                path,
                occurrences: agg.occurrences,
                max_len: agg.max_len,
                primitives_only: agg.primitives_only,
            })
            .collect(),
        record_count: state.records,
        projected_rows: state.projected_rows,
        projected_columns: n_cols,
        max_record_dims: state.max_record_dims,
        sample_rows,
        exploded: state.exploded,
        warnings,
    };
    Ok(JsonImportScan {
        preview,
        jsonl_index: collect_offsets.then_some(JsonlIndex {
            offsets,
            data_len: file_len,
        }),
        mode: Some(mode),
        kind: state.kind,
        columns: state.columns,
        inferred,
        nulls,
        fingerprint,
    })
}

// ----- the import pass -------------------------------------------------------------

/// Run the full import (pass 1 validate + union, then pass 2 emit): produce
/// a real document through the derived-document pipeline — an in-memory
/// editable document for small results, an indexed read-only document
/// (spilled through the F10 machinery under `cache_root`) for large ones or
/// when `force_indexed` is set. Inferred column schemas are attached, with
/// the null token registered on every column that contained explicit nulls,
/// so the missing/empty/null distinction from `schema::classify` holds in
/// the opened document. Any error — including invalid JSON discovered
/// mid-file — leaves no document and no stray cache files behind.
pub fn import(
    path: &Path,
    options: &JsonImportOptions,
    cache_root: &Path,
    doc_id: u64,
    ctx: Option<&JobCtx>,
) -> AppResult<Document> {
    let file_len = std::fs::metadata(path)?.len();
    if let Some(ctx) = ctx {
        ctx.set_total(file_len.saturating_mul(2));
        ctx.set_message("scanning JSON");
    }
    let scan = scan_impl(path, options, ctx, false)?;
    if scan.preview.needs_pointer {
        return Err(AppError::invalid(
            "select a record array (JSON Pointer) before importing this JSON object",
        ));
    }
    let mode = scan.mode.clone().expect("mode resolved by scan");
    let kind = scan.kind.expect("kind resolved by scan");
    if util::stat_fingerprint(path) != scan.fingerprint {
        return Err(AppError::Other(
            "the file changed on disk during the import; try again".into(),
        ));
    }
    let res = options.resolve()?;
    let budget = if options.force_indexed {
        0
    } else {
        SPILL_BUDGET
    };
    let mut builder =
        DerivedDocumentBuilder::new(scan.columns.clone(), cache_root.to_path_buf(), budget);
    let index_of: HashMap<&str, usize> = scan
        .columns
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i))
        .collect();
    let n_cols = scan.columns.len();
    if let Some(ctx) = ctx {
        ctx.set_message("building the document");
    }
    let mut emitted: u64 = 0;
    stream_records(path, &mode, ctx, None, &mut |_idx, loc, value| {
        let rec = flatten_record(kind, &value, &res, &loc)?;
        if rec.dims.len() > 1 && res.multi.is_none() {
            return Err(multi_choice_error(&rec, &loc));
        }
        expand_record(&rec, &res, &loc, &mut |row| {
            let mut cells = vec![res.missing_token.clone(); n_cols];
            for (path, cell) in &row.cells {
                let Some(&i) = index_of.get(path.as_str()) else {
                    return Err(AppError::Other(format!(
                        "the file changed during the import (unexpected field '{path}')"
                    )));
                };
                cells[i] = match cell {
                    CellVal::Null => res.null_token.clone(),
                    CellVal::Text(text, _) => text.clone(),
                };
            }
            builder.push_row(cells)?;
            emitted += 1;
            if emitted.is_multiple_of(CANCEL_EVERY_ROWS) {
                if let Some(ctx) = ctx {
                    ctx.check()?;
                }
            }
            Ok(true)
        })
    })?;
    let mut doc = builder.finish(doc_id, &mut |_| match ctx {
        Some(ctx) => ctx.check(),
        None => Ok(()),
    })?;

    // Attach the inferred schema: JSON-native types, plus the null token on
    // every column that contained explicit nulls (so classify() reports
    // NullToken for them, distinct from Empty/missing).
    let ids = doc.column_ids().to_vec();
    for (i, id) in ids.iter().enumerate() {
        let logical = scan.inferred.get(i).copied().unwrap_or(LogicalType::Text);
        let has_nulls = scan.nulls.get(i).copied().unwrap_or(0) > 0;
        if logical != LogicalType::Text || has_nulls {
            let mut schema = ColumnSchema::new(id.clone(), scan.columns[i].clone(), logical);
            if has_nulls {
                schema.null_tokens = vec![res.null_token.clone()];
            }
            doc.set_column_schema(schema);
        }
    }
    Ok(doc)
}

// ----- preview cache ---------------------------------------------------------------

/// Finished preview scans keyed by the job id that produced them. The front
/// end fetches the preview with `get_json_import_preview` after the
/// `job-finished` event (mirrors the recipe-batch report cache).
#[derive(Default)]
pub struct JsonImportPreviewCache(
    std::sync::Arc<std::sync::Mutex<HashMap<u64, JsonImportPreview>>>,
);

impl JsonImportPreviewCache {
    pub fn share(&self) -> std::sync::Arc<std::sync::Mutex<HashMap<u64, JsonImportPreview>>> {
        std::sync::Arc::clone(&self.0)
    }

    pub fn get(&self, job_id: u64) -> Option<JsonImportPreview> {
        self.0.lock().ok()?.get(&job_id).cloned()
    }
}

// ----- tests -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;
    use crate::schema::{classify, CellState};
    use std::path::PathBuf;

    fn write(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn scan_str(
        name: &str,
        contents: &str,
        options: &JsonImportOptions,
    ) -> AppResult<JsonImportScan> {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), name, contents.as_bytes());
        scan(&path, options, None)
    }

    /// Import `contents` and keep the tempdir (indexed documents read their
    /// backing file lazily) alive alongside the document.
    fn import_str(
        name: &str,
        contents: &str,
        options: &JsonImportOptions,
    ) -> AppResult<(tempfile::TempDir, Document)> {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), name, contents.as_bytes());
        let cache = dir.path().join("cache");
        import(&path, options, &cache, 1, None).map(|doc| (dir, doc))
    }

    fn options() -> JsonImportOptions {
        JsonImportOptions::default()
    }

    fn err_of<T>(result: AppResult<T>) -> String {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        }
    }

    fn preview_col<'a>(scan: &'a JsonImportScan, name: &str) -> &'a PreviewColumn {
        scan.preview
            .columns
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no column {name}"))
    }

    // ----- path escaping ------------------------------------------------------

    #[test]
    fn key_escaping_round_trips() {
        assert_eq!(escape_key("plain"), "plain");
        assert_eq!(escape_key("a.b"), "a\\.b");
        assert_eq!(escape_key("a\\b"), "a\\\\b");
        assert_eq!(split_path("a.b"), vec!["a", "b"]);
        assert_eq!(split_path("a\\.b"), vec!["a.b"]);
        assert_eq!(split_path("a\\\\b.c"), vec!["a\\b", "c"]);
        assert_eq!(
            split_path(&format!("{}.{}", escape_key("x.y"), escape_key("z\\w"))),
            vec!["x.y", "z\\w"]
        );
    }

    // ----- JSONL framing ------------------------------------------------------

    #[test]
    fn jsonl_framing_offsets_survive_any_chunking() {
        let contents = "a\nbb\n\nccc\r\n  \ndddd";
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "t.txt", contents.as_bytes());
        for chunk in [1usize, 2, 3, 7, 64 * 1024] {
            let mut seen: Vec<(u64, u64, u64, String)> = Vec::new();
            let (data_len, peak_buffer) =
                stream_jsonl(&path, chunk, None, &mut |idx, offset, line_no, bytes| {
                    seen.push((
                        idx,
                        offset,
                        line_no,
                        String::from_utf8(bytes.to_vec()).unwrap(),
                    ));
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                seen,
                vec![
                    (0, 0, 1, "a".into()),
                    (1, 2, 2, "bb".into()),
                    (2, 6, 4, "ccc".into()),
                    (3, 14, 6, "dddd".into()),
                ],
                "chunk {chunk}"
            );
            assert_eq!(data_len, contents.len() as u64, "chunk {chunk}");
            // Bounded memory: the carry buffer never exceeds the longest
            // line plus one chunk (never the whole file for small chunks).
            let longest = 5; // "ccc\r\n"
            assert!(
                peak_buffer <= longest + chunk,
                "chunk {chunk}: peak {peak_buffer}"
            );
        }
    }

    #[test]
    fn jsonl_framing_strips_utf8_bom() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = UTF8_BOM.to_vec();
        bytes.extend_from_slice(b"{\"a\":1}\n{\"a\":2}\n");
        let path = write(dir.path(), "t.jsonl", &bytes);
        for chunk in [1usize, 4, 1024] {
            let mut offsets = Vec::new();
            stream_jsonl(&path, chunk, None, &mut |_idx, offset, _line, bytes| {
                offsets.push(offset);
                assert!(bytes.starts_with(b"{"));
                Ok(())
            })
            .unwrap();
            assert_eq!(offsets, vec![3, 11], "chunk {chunk}");
        }
    }

    #[test]
    fn jsonl_index_reads_windows_by_offset() {
        let contents = "{\"a\":1}\n{\"a\":22}\n\n{\"a\":333}\n";
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "t.jsonl", contents.as_bytes());
        let scan = scan(&path, &options(), None).unwrap();
        let index = scan.jsonl_index.as_ref().unwrap();
        assert_eq!(index.n_records(), 3);
        assert_eq!(index.offsets, vec![0, 8, 18]);

        let rows = index.read_records(&path, 1, 3).unwrap();
        assert_eq!(rows, vec![b"{\"a\":22}".to_vec(), b"{\"a\":333}".to_vec()]);
        assert!(index.read_records(&path, 3, 5).unwrap().is_empty());
        assert!(index.read_records(&path, 0, 0).unwrap().is_empty());

        // A shrunk file errors instead of returning garbage.
        std::fs::write(&path, b"{\"a\":1}\n").unwrap();
        assert!(index.read_records(&path, 1, 3).is_err());
    }

    // ----- shape detection ----------------------------------------------------

    #[test]
    fn detects_the_four_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let cases: Vec<(&str, &str, DetectedShape)> = vec![
            ("a.json", "[{\"a\":1}]", DetectedShape::ObjectArray),
            ("b.json", "  [ [1,2], [3] ]", DetectedShape::ArrayOfArrays),
            ("c.json", "[1,2,3]", DetectedShape::PrimitiveArray),
            ("d.jsonl", "{\"a\":1}", DetectedShape::JsonLines),
            ("e.ndjson", "[1]\n[2]", DetectedShape::JsonLines),
            ("f.json", "{\"a\":1}\n{\"a\":2}\n", DetectedShape::JsonLines),
            ("g.json", "{\n  \"a\": 1\n}", DetectedShape::ObjectDocument),
            (
                "h.json",
                "{\"a\":{\"b\":[1]}}",
                DetectedShape::ObjectDocument,
            ),
            ("i.json", "42", DetectedShape::ScalarDocument),
        ];
        for (name, contents, expected) in cases {
            let path = write(dir.path(), name, contents.as_bytes());
            let info = detect_shape(&path).unwrap();
            assert_eq!(info.shape, expected, "{name}");
        }
    }

    #[test]
    fn rejects_utf16_and_empty_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in "[{\"a\":1}]".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        let path = write(dir.path(), "u16.json", &utf16);
        assert!(err_of(detect_shape(&path)).contains("UTF-8"));

        let empty = write(dir.path(), "empty.json", b"");
        assert!(err_of(detect_shape(&empty)).contains("empty"));
        let blank = write(dir.path(), "blank.json", b"   \n ");
        assert!(err_of(detect_shape(&blank)).contains("whitespace"));
    }

    #[test]
    fn object_document_candidates_are_ranked_and_escaped() {
        let dir = tempfile::tempdir().unwrap();
        let contents = r#"{
  "meta": {"version": 1},
  "data": {
    "items": [{"a": 1}, {"a": 2}, {"a": 3}],
    "tags": ["x", "y"],
    "odd/key": [{"b": 1}]
  }
}"#;
        let path = write(dir.path(), "doc.json", contents.as_bytes());
        let info = detect_shape(&path).unwrap();
        assert_eq!(info.shape, DetectedShape::ObjectDocument);
        let pointers: Vec<&str> = info.candidates.iter().map(|c| c.pointer.as_str()).collect();
        // Object arrays outrank primitive arrays; longer first among equals.
        assert_eq!(pointers[0], "/data/items");
        assert_eq!(info.candidates[0].records, 3);
        assert_eq!(info.candidates[0].element_kind, "object");
        assert_eq!(pointers[1], "/data/odd~1key", "RFC 6901 escaping");
        assert!(pointers.contains(&"/data/tags"));
        let tags = info
            .candidates
            .iter()
            .find(|c| c.pointer == "/data/tags")
            .unwrap();
        assert_eq!(tags.element_kind, "primitive");
    }

    // ----- pointer resolution -------------------------------------------------

    #[test]
    fn pointer_streams_a_nested_record_array() {
        let mut opts = options();
        opts.pointer = Some("/data/items".into());
        let scan = scan_str(
            "doc.json",
            r#"{"meta": {"huge": [1,2,3]}, "data": {"items": [{"a": 1}, {"a": 2, "b": "x"}]}}"#,
            &opts,
        )
        .unwrap();
        assert_eq!(scan.preview.record_count, 2);
        assert_eq!(scan.preview.pointer.as_deref(), Some("/data/items"));
        assert_eq!(scan.columns, vec!["a", "b"]);
        assert_eq!(preview_col(&scan, "b").missing, 1);
    }

    #[test]
    fn pointer_escapes_and_array_indices_resolve() {
        let mut opts = options();
        opts.pointer = Some("/groups/1/a~1b~0c".into());
        let scan = scan_str(
            "doc.json",
            r#"{"groups": [{"skip": true}, {"a/b~c": [{"v": 1}, {"v": 2}]}]}"#,
            &opts,
        )
        .unwrap();
        assert_eq!(scan.preview.record_count, 2);
        assert_eq!(scan.columns, vec!["v"]);
    }

    #[test]
    fn pointer_failures_report_clearly() {
        let doc = r#"{"data": {"items": [{"a": 1}], "n": 5}}"#;
        let mut opts = options();
        opts.pointer = Some("/data/missing".into());
        assert!(err_of(scan_str("d.json", doc, &opts)).contains("was not found"));

        opts.pointer = Some("/data".into());
        assert!(err_of(scan_str("d.json", doc, &opts)).contains("object, not an array"));

        opts.pointer = Some("/data/n".into());
        assert!(err_of(scan_str("d.json", doc, &opts)).contains("expected an array of records"));

        opts.pointer = Some("/data/items/5".into());
        assert!(err_of(scan_str("d.json", doc, &opts)).contains("out of range"));

        opts.pointer = Some("no-slash".into());
        assert!(err_of(scan_str("d.json", doc, &opts)).contains("must start with '/'"));

        opts.pointer = Some("/bad~2escape".into());
        assert!(err_of(scan_str("d.json", doc, &opts)).contains("escape"));
    }

    #[test]
    fn object_document_without_pointer_needs_one() {
        let doc = r#"{"data": {"items": [{"a": 1}]}}"#;
        let scan = scan_str("d.json", doc, &options()).unwrap();
        assert!(scan.preview.needs_pointer);
        assert_eq!(scan.preview.candidates[0].pointer, "/data/items");
        assert!(scan.preview.columns.is_empty());

        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "d.json", doc.as_bytes());
        let err = err_of(import(&path, &options(), &dir.path().join("c"), 1, None));
        assert!(err.contains("JSON Pointer"), "{err}");
    }

    // ----- key union ----------------------------------------------------------

    #[test]
    fn key_union_is_first_seen_then_alphabetical_for_late_keys() {
        // Record 1 establishes document order (z before a); records 2 and 3
        // introduce late keys, appended alphabetically per record batch.
        let scan = scan_str(
            "u.jsonl",
            "{\"z\": 1, \"a\": 2}\n{\"m\": 3, \"c\": 4, \"a\": 5}\n{\"b\": 6}\n",
            &options(),
        )
        .unwrap();
        assert_eq!(scan.columns, vec!["z", "a", "c", "m", "b"]);
    }

    #[test]
    fn duplicate_keys_in_one_object_last_wins() {
        let (_dir, doc) = import_str("dup.jsonl", "{\"a\": 1, \"a\": 2}\n", &options()).unwrap();
        assert_eq!(doc.headers(), &["a"]);
        assert_eq!(doc.rows()[0], vec!["2".to_string()]);
    }

    // ----- missing vs null ----------------------------------------------------

    #[test]
    fn missing_and_null_stay_distinct_end_to_end() {
        let contents = "{\"a\": 1, \"b\": \"x\"}\n{\"a\": 2}\n{\"a\": 3, \"b\": null}\n";
        let scan = scan_str("mn.jsonl", contents, &options()).unwrap();
        let b = preview_col(&scan, "b");
        assert_eq!((b.present, b.nulls, b.missing), (1, 1, 1));

        let (_dir, doc) = import_str("mn.jsonl", contents, &options()).unwrap();
        assert_eq!(doc.headers(), &["a", "b"]);
        let rows = doc.rows();
        assert_eq!(rows[0][1], "x");
        assert_eq!(rows[1][1], "", "missing -> missing token (empty)");
        assert_eq!(rows[2][1], "null", "explicit null -> null token");
        assert_ne!(rows[1][1], rows[2][1], "distinct in the grid");

        // And distinct through schema classification: the import attached
        // the null token to column b.
        let schema = doc.column_schema_at(1).expect("schema attached");
        assert_eq!(schema.null_tokens, vec!["null".to_string()]);
        assert_eq!(classify(Some(""), schema), CellState::Empty);
        assert_eq!(classify(Some("null"), schema), CellState::NullToken);
    }

    #[test]
    fn custom_tokens_apply_and_equal_tokens_are_rejected() {
        let mut opts = options();
        opts.null_token = "<NULL>".into();
        opts.missing_token = "<MISSING>".into();
        let contents = "{\"a\": null}\n{}\n{\"a\": \"x\"}\n";
        let (_dir, doc) = import_str("tok.jsonl", contents, &opts).unwrap();
        let rows = doc.rows();
        assert_eq!(rows[0][0], "<NULL>");
        assert_eq!(rows[1][0], "<MISSING>");
        assert_eq!(rows[2][0], "x");

        let mut bad = options();
        bad.missing_token = "null".into();
        assert!(err_of(scan_str("tok.jsonl", contents, &bad)).contains("must differ"));
    }

    #[test]
    fn token_collisions_are_counted_as_warnings() {
        let contents = "{\"a\": \"null\", \"b\": \"\"}\n";
        let scan = scan_str("col.jsonl", contents, &options()).unwrap();
        assert_eq!(scan.preview.warnings.len(), 2);
        assert!(scan.preview.warnings[0].contains("null token"));
        assert!(scan.preview.warnings[1].contains("missing-value"));
    }

    // ----- nested objects -----------------------------------------------------

    #[test]
    fn flatten_uses_escaped_path_names() {
        let contents = "{\"a.b\": 1, \"a\": {\"b\": 2, \"c\": {\"d\": 3}}}\n";
        let (_dir, doc) = import_str("nest.jsonl", contents, &options()).unwrap();
        assert_eq!(doc.headers(), &["a\\.b", "a.b", "a.c.d"]);
        assert_eq!(doc.rows()[0], vec!["1", "2", "3"]);
    }

    #[test]
    fn preserve_json_keeps_compact_document_order() {
        let mut opts = options();
        opts.nested_policy = NestedPolicy::PreserveJson;
        let contents = "{\"id\": 1, \"o\": {\"z\": 1, \"a\": [true, null]}}\n";
        let (_dir, doc) = import_str("pj.jsonl", contents, &opts).unwrap();
        assert_eq!(doc.headers(), &["id", "o"]);
        assert_eq!(doc.rows()[0][1], r#"{"z":1,"a":[true,null]}"#);
        // Preserved subtrees infer the JSON logical type.
        let schema = doc.column_schema_at(1).expect("schema");
        assert_eq!(schema.logical_type, LogicalType::Json);
    }

    #[test]
    fn ignore_paths_drop_subtrees() {
        let mut opts = options();
        opts.ignore_paths = vec!["secret".into(), "o.token".into()];
        let contents = "{\"a\": 1, \"secret\": {\"k\": 2}, \"o\": {\"token\": 3, \"keep\": 4}}\n";
        let (_dir, doc) = import_str("ig.jsonl", contents, &opts).unwrap();
        assert_eq!(doc.headers(), &["a", "o.keep"]);
        assert_eq!(doc.rows()[0], vec!["1", "4"]);
    }

    #[test]
    fn empty_objects_flatten_to_missing() {
        let contents = "{\"a\": {}, \"b\": 1}\n";
        let scan = scan_str("eo.jsonl", contents, &options()).unwrap();
        assert_eq!(scan.columns, vec!["b"], "empty object contributes no cells");
        assert!(scan.preview.nested_object_paths.contains(&"a".to_string()));
    }

    // ----- array policies -----------------------------------------------------

    #[test]
    fn arrays_preserve_as_json_by_default() {
        let contents = "{\"a\": 1, \"t\": [1, \"x\", null]}\n";
        let (_dir, doc) = import_str("ap.jsonl", contents, &options()).unwrap();
        assert_eq!(doc.rows()[0][1], r#"[1,"x",null]"#);
    }

    #[test]
    fn join_concatenates_primitives_and_rejects_nested() {
        let mut opts = options();
        opts.array_policy = ArrayPolicy::Join;
        assert!(err_of(scan_str("j.jsonl", "{\"t\": [1]}\n", &opts)).contains("joinSeparator"));

        opts.join_separator = Some("|".into());
        let (_dir, doc) =
            import_str("j.jsonl", "{\"t\": [1, null, \"z\", true]}\n", &opts).unwrap();
        assert_eq!(doc.rows()[0][0], "1|null|z|true");

        let err = err_of(scan_str("j.jsonl", "{\"t\": [{\"x\": 1}]}\n", &opts));
        assert!(err.contains("cannot be joined"), "{err}");
    }

    #[test]
    fn reject_policy_fails_on_any_array() {
        let mut opts = options();
        opts.array_policy = ArrayPolicy::Reject;
        let err = err_of(scan_str("r.jsonl", "{\"ok\": 1}\n{\"t\": [1]}\n", &opts));
        assert!(err.contains("record 2"), "{err}");
        assert!(err.contains("'t'"), "{err}");
        assert!(err.contains("reject"), "{err}");
    }

    #[test]
    fn explode_turns_elements_into_rows() {
        let mut opts = options();
        opts.array_policy = ArrayPolicy::Explode;
        let contents = "{\"id\": 1, \"t\": [\"a\", \"b\"]}\n{\"id\": 2, \"t\": []}\n";
        let scan = scan_str("e.jsonl", contents, &opts).unwrap();
        assert_eq!(scan.preview.record_count, 2);
        assert_eq!(
            scan.preview.projected_rows, 3,
            "2 elements + 1 empty-array row"
        );
        assert!(scan.preview.exploded);

        let (_dir, doc) = import_str("e.jsonl", contents, &opts).unwrap();
        assert_eq!(doc.headers(), &["id", "t"]);
        let rows = doc.rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], vec!["1", "a"]);
        assert_eq!(rows[1], vec!["1", "b"]);
        assert_eq!(
            rows[2],
            vec!["2", ""],
            "empty array -> one row, field missing"
        );
    }

    #[test]
    fn exploded_object_elements_flatten_into_path_columns() {
        let mut opts = options();
        opts.array_policy = ArrayPolicy::Explode;
        let contents = "{\"id\": 1, \"items\": [{\"sku\": \"a\", \"qty\": 2}, {\"sku\": \"b\"}]}\n";
        let (_dir, doc) = import_str("eo.jsonl", contents, &opts).unwrap();
        assert_eq!(doc.headers(), &["id", "items.sku", "items.qty"]);
        let rows = doc.rows();
        assert_eq!(rows[0], vec!["1", "a", "2"]);
        assert_eq!(rows[1], vec!["1", "b", ""], "short element: qty missing");
    }

    #[test]
    fn dual_array_explode_requires_an_explicit_choice() {
        let mut opts = options();
        opts.array_policy = ArrayPolicy::Explode;
        // A SINGLE record with two arrays that co-occur.
        let contents = "{\"x\": [1, 2], \"y\": [\"a\", \"b\"]}\n";
        // The scan is permissive so the dialog can prompt inline: it succeeds
        // and reports the co-occurrence (max_record_dims == 2) instead of
        // erroring.
        let scan = scan_str("m.jsonl", contents, &opts).unwrap();
        assert_eq!(scan.preview.max_record_dims, 2);
        // The import/emit pass is the authority and rejects the missing choice.
        let err = err_of(import_str("m.jsonl", contents, &opts));
        assert!(err.contains("cartesian or zip"), "{err}");
        assert!(err.contains("x, y"), "{err}");
    }

    #[test]
    fn heterogeneous_arrays_that_never_co_occur_need_no_multi_choice() {
        // Two DIFFERENT array fields, each alone in its own record: the
        // per-record explode dimension is always 1, so no multiArray choice is
        // needed even though the document-wide array list has two entries.
        let mut opts = options();
        opts.array_policy = ArrayPolicy::Explode;
        let contents = "{\"x\": [1, 2]}\n{\"y\": [\"a\", \"b\"]}\n";
        let scan = scan_str("h.jsonl", contents, &opts).unwrap();
        assert_eq!(scan.preview.array_fields.len(), 2, "both arrays reported");
        assert_eq!(
            scan.preview.max_record_dims, 1,
            "no single record explodes two dimensions"
        );
        // And the import runs with no multiArray mode set.
        let (_dir, doc) = import_str("h.jsonl", contents, &opts).unwrap();
        assert_eq!(doc.rows().len(), 4);
    }

    #[test]
    fn bom_prefixed_array_document_scans_and_imports() {
        // A UTF-8 BOM in front of a plain array document must not break the
        // streaming parse (regression: stream_doc fed the raw file, BOM and
        // all, to serde_json).
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = UTF8_BOM.to_vec();
        bytes.extend_from_slice(b"[{\"a\":1},{\"a\":2}]");
        let path = write(dir.path(), "b.json", &bytes);
        let opts = options();
        let scan = scan(&path, &opts, None).unwrap();
        assert_eq!(scan.preview.record_count, 2);
        assert_eq!(preview_col(&scan, "a").present, 2);
        let cache = dir.path().join("cache");
        let doc = import(&path, &opts, &cache, 1, None).unwrap();
        assert_eq!(doc.rows().len(), 2);
    }

    #[test]
    fn bom_prefixed_object_document_with_pointer_imports() {
        // BOM + object document reached through a JSON Pointer.
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = UTF8_BOM.to_vec();
        bytes.extend_from_slice(b"{\"items\":[{\"a\":1},{\"a\":2},{\"a\":3}]}");
        let path = write(dir.path(), "bo.json", &bytes);
        let mut opts = options();
        opts.pointer = Some("/items".to_string());
        let scan = scan(&path, &opts, None).unwrap();
        assert_eq!(scan.preview.record_count, 3);
        let cache = dir.path().join("cache");
        let doc = import(&path, &opts, &cache, 1, None).unwrap();
        assert_eq!(doc.rows().len(), 3);
    }

    #[test]
    fn cartesian_explode_crosses_dimensions() {
        let mut opts = options();
        opts.array_policy = ArrayPolicy::Explode;
        opts.multi_array = Some(MultiArrayMode::Cartesian);
        let contents = "{\"x\": [1, 2], \"y\": [\"a\", \"b\"]}\n";
        let scan = scan_str("c.jsonl", contents, &opts).unwrap();
        assert_eq!(scan.preview.projected_rows, 4);
        let (_dir, doc) = import_str("c.jsonl", contents, &opts).unwrap();
        let rows = doc.rows();
        assert_eq!(rows.len(), 4);
        // The last dimension varies fastest.
        assert_eq!(rows[0], vec!["1", "a"]);
        assert_eq!(rows[1], vec!["1", "b"]);
        assert_eq!(rows[2], vec!["2", "a"]);
        assert_eq!(rows[3], vec!["2", "b"]);
    }

    #[test]
    fn zip_explode_aligns_and_pads_missing() {
        let mut opts = options();
        opts.array_policy = ArrayPolicy::Explode;
        opts.multi_array = Some(MultiArrayMode::Zip);
        let contents = "{\"x\": [1, 2, 3], \"y\": [\"a\"]}\n";
        let scan = scan_str("z.jsonl", contents, &opts).unwrap();
        assert_eq!(scan.preview.projected_rows, 3, "zip runs to the longest");
        let (_dir, doc) = import_str("z.jsonl", contents, &opts).unwrap();
        let rows = doc.rows();
        assert_eq!(rows[0], vec!["1", "a"]);
        assert_eq!(rows[1], vec!["2", ""], "shorter array pads as missing");
        assert_eq!(rows[2], vec!["3", ""]);
    }

    #[test]
    fn nested_arrays_inside_exploded_elements_are_rejected() {
        let mut opts = options();
        opts.array_policy = ArrayPolicy::Explode;
        let err = err_of(scan_str("n.jsonl", "{\"t\": [[1, 2]]}\n", &opts));
        assert!(err.contains("nested explode is not supported"), "{err}");
    }

    // ----- preview ------------------------------------------------------------

    #[test]
    fn preview_reports_shapes_counts_types_and_samples() {
        let mut lines = String::new();
        for i in 0..80 {
            lines.push_str(&format!(
                "{{\"i\": {i}, \"f\": {i}.5, \"b\": true, \"s\": \"x{i}\", \"o\": {{\"n\": {i}}}, \"t\": [1]}}\n"
            ));
        }
        let scan = scan_str("p.jsonl", &lines, &options()).unwrap();
        let p = &scan.preview;
        assert_eq!(p.shape, DetectedShape::JsonLines);
        assert!(!p.needs_pointer);
        assert_eq!(p.record_kind.as_deref(), Some("object"));
        assert_eq!(p.record_count, 80);
        assert_eq!(p.projected_rows, 80);
        assert_eq!(p.projected_columns, 6);
        assert_eq!(p.sample_rows.len(), SAMPLE_ROWS, "samples are bounded");
        assert_eq!(p.sample_rows[0].len(), 6);

        assert_eq!(preview_col(&scan, "i").inferred_type, LogicalType::Integer);
        assert_eq!(preview_col(&scan, "f").inferred_type, LogicalType::Float);
        assert_eq!(preview_col(&scan, "b").inferred_type, LogicalType::Boolean);
        assert_eq!(preview_col(&scan, "s").inferred_type, LogicalType::Text);
        assert_eq!(preview_col(&scan, "t").inferred_type, LogicalType::Json);

        assert_eq!(p.nested_object_paths, vec!["o".to_string()]);
        assert_eq!(p.array_fields.len(), 1);
        assert_eq!(p.array_fields[0].path, "t");
        assert_eq!(p.array_fields[0].occurrences, 80);
        assert_eq!(p.array_fields[0].max_len, 1);
        assert!(p.array_fields[0].primitives_only);
    }

    #[test]
    fn array_of_arrays_gets_positional_columns() {
        let contents = "[[1, \"a\"], [2, \"b\", true], [3]]";
        let scan = scan_str("aa.json", contents, &options()).unwrap();
        assert_eq!(scan.preview.shape, DetectedShape::ArrayOfArrays);
        assert_eq!(scan.preview.record_kind.as_deref(), Some("array"));
        assert_eq!(scan.columns, vec!["Column 1", "Column 2", "Column 3"]);
        assert_eq!(preview_col(&scan, "Column 3").missing, 2);

        let (_dir, doc) = import_str("aa.json", contents, &options()).unwrap();
        assert_eq!(doc.rows()[1], vec!["2", "b", "true"]);
        assert_eq!(doc.rows()[2], vec!["3", "", ""]);
    }

    #[test]
    fn primitive_records_become_a_value_column() {
        let scan = scan_str("v.jsonl", "1\n\"two\"\n3.5\n", &options()).unwrap();
        assert_eq!(scan.preview.record_kind.as_deref(), Some("value"));
        assert_eq!(scan.columns, vec!["value"]);
        assert_eq!(scan.preview.record_count, 3);
    }

    #[test]
    fn mixed_record_kinds_error_with_the_record_location() {
        let err = err_of(scan_str("mix.jsonl", "{\"a\": 1}\n[1, 2]\n", &options()));
        assert!(err.contains("record 2 (line 2)"), "{err}");
        assert!(err.contains("share one shape"), "{err}");
    }

    // ----- error locations ----------------------------------------------------

    #[test]
    fn jsonl_syntax_errors_report_offset_line_column_context() {
        let contents = "{\"ok\": 1}\n{\"a\": nope}\n";
        let err = err_of(scan_str("bad.jsonl", contents, &options()));
        assert!(err.contains("invalid JSON at byte offset "), "{err}");
        assert!(err.contains("(line 2, column "), "{err}");
        assert!(err.contains("nope"), "{err}");
        // The reported offset lands inside the second line.
        let offset: u64 = err
            .split("byte offset ")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .and_then(|s| s.parse().ok())
            .unwrap();
        assert!((10..contents.len() as u64).contains(&offset), "{err}");
    }

    #[test]
    fn document_syntax_errors_report_absolute_locations() {
        let contents = "[1, 2, oops]";
        let err = err_of(scan_str("bad.json", contents, &options()));
        let expected = contents.find("oops").unwrap() as u64;
        assert!(
            err.contains(&format!("byte offset {expected} ")),
            "expected offset {expected} in: {err}"
        );
        assert!(err.contains("line 1"), "{err}");
        assert!(err.contains("oops"), "{err}");
    }

    #[test]
    fn trailing_garbage_after_the_document_is_invalid() {
        let err = err_of(scan_str("t.json", "[{\"a\": 1}] trailing", &options()));
        assert!(err.contains("trailing"), "{err}");
    }

    // ----- import -------------------------------------------------------------

    #[test]
    fn small_imports_are_editable_unsaved_documents() {
        let (_dir, doc) = import_str(
            "small.json",
            r#"[{"a": 1, "b": "x"}, {"a": 2}]"#,
            &options(),
        )
        .unwrap();
        assert!(doc.is_editable());
        assert!(doc.is_dirty(), "imported documents start unsaved");
        assert!(doc.meta().path.is_none());
        assert_eq!(doc.headers(), &["a", "b"]);
        assert_eq!(doc.n_rows(), 2);
        // Inferred schema: integers on a.
        let schema = doc.column_schema_at(0).expect("schema");
        assert_eq!(schema.logical_type, LogicalType::Integer);
    }

    #[test]
    fn force_indexed_produces_a_read_only_document() {
        let mut opts = options();
        opts.force_indexed = true;
        let mut lines = String::new();
        for i in 0..500 {
            lines.push_str(&format!("{{\"a\": {i}, \"b\": \"v{i}\"}}\n"));
        }
        let (_dir, doc) = import_str("big.jsonl", &lines, &opts).unwrap();
        assert!(!doc.is_editable(), "forced indexed backing");
        assert_eq!(doc.n_rows(), 500);
        let rows = doc.fetch_rows(&[0, 499]).unwrap();
        assert_eq!(rows[0], vec!["0", "v0"]);
        assert_eq!(rows[1], vec!["499", "v499"]);
    }

    #[test]
    fn a_root_array_streams_through_the_same_pipeline() {
        let mut opts = options();
        opts.force_indexed = true;
        let mut contents = String::from("[");
        for i in 0..3000 {
            if i > 0 {
                contents.push(',');
            }
            contents.push_str(&format!("{{\"n\": {i}}}"));
        }
        contents.push(']');
        let (_dir, doc) = import_str("arr.json", &contents, &opts).unwrap();
        assert!(!doc.is_editable());
        assert_eq!(doc.n_rows(), 3000);
        assert_eq!(doc.fetch_rows(&[2999]).unwrap()[0], vec!["2999"]);
    }

    #[test]
    fn invalid_input_never_creates_a_partial_document() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = String::new();
        for i in 0..200 {
            lines.push_str(&format!("{{\"a\": {i}}}\n"));
        }
        lines.push_str("{\"a\": broken\n");
        let path = write(dir.path(), "bad.jsonl", lines.as_bytes());
        let cache = dir.path().join("cache");
        let mut opts = options();
        opts.force_indexed = true;
        assert!(import(&path, &opts, &cache, 1, None).is_err());
        // The scan failed before anything was built: no cache leftovers.
        let leftovers = std::fs::read_dir(&cache).map(|d| d.count()).unwrap_or(0);
        assert_eq!(leftovers, 0, "no partial document, no stray cache dirs");
    }

    #[test]
    fn empty_inputs_error_instead_of_opening() {
        assert!(err_of(scan_str("e.json", "[]", &options())).contains("no records"));
        assert!(err_of(scan_str("e.jsonl", "\n\n", &options())).contains("whitespace"));
        let err = err_of(scan_str("e2.jsonl", "{}\n{}\n", &options()));
        assert!(err.contains("no fields"), "{err}");
    }

    #[test]
    fn cancellation_aborts_the_import() {
        let registry = JobRegistry::default();
        let ctx = registry.begin("test", None, |_| {});
        registry.cancel(ctx.id);
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "c.jsonl", b"{\"a\": 1}\n{\"a\": 2}\n");
        let result = import(&path, &options(), &dir.path().join("cache"), 1, Some(&ctx));
        assert!(matches!(result, Err(AppError::Cancelled)));
    }

    #[test]
    fn too_many_columns_are_rejected() {
        let mut record = String::from("{");
        for i in 0..(MAX_COLUMNS + 1) {
            if i > 0 {
                record.push(',');
            }
            record.push_str(&format!("\"k{i}\": 1"));
        }
        record.push('}');
        let err = err_of(scan_str("wide.jsonl", &format!("{record}\n"), &options()));
        assert!(err.contains("columns"), "{err}");
    }

    #[test]
    fn numbers_normalise_through_ieee_doubles() {
        let (_dir, doc) = import_str(
            "num.jsonl",
            "{\"a\": 1.0, \"b\": 1e2, \"c\": 7, \"d\": -0.5}\n",
            &options(),
        )
        .unwrap();
        assert_eq!(doc.rows()[0], vec!["1.0", "100.0", "7", "-0.5"]);
    }

    #[test]
    fn jsonl_pointer_is_rejected() {
        let mut opts = options();
        opts.pointer = Some("/data".into());
        let err = err_of(scan_str("p.jsonl", "{\"a\": 1}\n{\"a\": 2}\n", &opts));
        assert!(err.contains("do not apply to JSON Lines"), "{err}");
    }
}
