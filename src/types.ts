// TypeScript mirrors of the Rust DTOs (see `src-tauri/src/dto.rs`). All fields
// are camelCase to match the serde `rename_all = "camelCase"` wire format.

export type LineEnding = "lf" | "crlf";
export type QuoteStyle = "minimal" | "always";

export interface DocumentMeta {
  id: number;
  path: string | null;
  fileName: string;
  rowCount: number;
  totalRowCount: number;
  filtered: boolean;
  colCount: number;
  headers: string[];
  /**
   * Stable logical column IDs (F12), in lockstep with `headers`. Assigned
   * positionally at parse ("c0".."cN-1") and preserved through renames,
   * reorders and undo/redo, so named views survive structural edits.
   */
  columnIds: string[];
  /** Whether a non-destructive view sort (F12) is currently applied. */
  viewSorted?: boolean;
  hasHeaderRow: boolean;
  delimiter: string;
  encoding: string;
  hadBom: boolean;
  lineEnding: LineEnding;
  dirty: boolean;
  canUndo: boolean;
  canRedo: boolean;
  /** Read-only follow/tail mode (F19). */
  follow?: boolean;
  /**
   * Monotonically increasing revision, bumped on every mutation. Previews and
   * deferred operations echo this back as `expectedRevision` and are rejected
   * by the backend when the document has moved on.
   */
  revision: number;
  /** Row storage: "editable" (in memory) or "indexedReadOnly" (F10). */
  backing: DocumentBacking;
  /** Where the document came from when opened out of an archive (F17). */
  archive: ArchiveOrigin | null;
}

export type DocumentBacking = "editable" | "indexedReadOnly";

/** Archive provenance for a document opened from .gz / .zip (F17). */
export interface ArchiveOrigin {
  archivePath: string;
  entryName: string | null;
  archiveFingerprint: FileFingerprint | null;
}

/** One candidate entry inside a ZIP archive (F17). */
export interface ZipEntryInfo {
  name: string;
  compressedSize: number;
  uncompressedSize: number;
  ratio: number;
  encrypted: boolean;
  likelyDelimiter: string | null;
  likelyEncoding: string | null;
}

/** Handles returned by start_archive_extract (F17). */
export interface ArchiveExtractStart {
  jobId: number;
  token: number;
}

/** Incremental progress for a background job (event: "job-progress"). */
export interface JobProgress {
  jobId: number;
  docId: number | null;
  kind: string;
  processed: number;
  total: number | null;
  bytesWritten: number | null;
  part: number | null;
  message: string | null;
}

export type JobStatus = "done" | "cancelled" | "failed";

/** Terminal state of a background job (event: "job-finished"). */
export interface JobFinished {
  jobId: number;
  docId: number | null;
  kind: string;
  status: JobStatus;
  error: string | null;
}

export type DiagnosticSeverity = "error" | "warning" | "info";

/** A pointer at (or description of) one place affected by a diagnostic. */
export interface DiagnosticSample {
  /** Data-row index in the current document, when the issue maps to one. */
  row: number | null;
  /** Column index, when the issue maps to one. */
  col: number | null;
  /** Truncated cell/header value for display. */
  value: string | null;
  /** Extra context (e.g. "line 1042 had 3 fields (expected 5)"). */
  note: string | null;
}

export interface DiagnosticIssue {
  /** Stable identifier: the kind, plus ":column" when column-scoped. */
  id: string;
  kind: string;
  severity: DiagnosticSeverity;
  title: string;
  description: string;
  affectedCount: number;
  samples: DiagnosticSample[];
  suggestedAction: string | null;
  /** Whether "filter to affected rows" is meaningful for this issue. */
  rowFilterable: boolean;
}

export interface DiagnosticsReport {
  docId: number;
  /** Document revision this report was computed against. */
  revision: number;
  /** Issues describing the imported source file. */
  source: DiagnosticIssue[];
  /** Issues describing the current in-memory document. */
  current: DiagnosticIssue[];
}

/** Identity snapshot of a backing file, for external-change detection. */
export interface FileFingerprint {
  size: number;
  modifiedAtMs: number;
}

/** One source record whose field count differed from the modal count. */
export interface RaggedSample {
  /** 1-based line number in the source file where the record starts. */
  line: number;
  fields: number;
}

/** One setting whose value would change under a proposed reparse. */
export interface ReparseDiff {
  /** Machine-readable field name (e.g. "delimiter", "rowCount"). */
  field: string;
  current: string;
  proposed: string;
}

/** Non-destructive preview of reopening the source file with new settings. */
export interface ReparsePreview {
  /** First records exactly as parsed (header row included when detected). */
  records: string[][];
  delimiter: string;
  encoding: string;
  hadBom: boolean;
  lineEnding: LineEnding;
  hasHeaderRow: boolean;
  /** Data rows the reopened document would have (header excluded). */
  rowCount: number;
  colCount: number;
  hadDecodeErrors: boolean;
  raggedTotal: number;
  modalFieldCount: number;
  raggedSamples: RaggedSample[];
  /** Settings/shape that differ from the current interpretation. */
  differences: ReparseDiff[];
  /** Echo back to applyReparse; rejected when the document moved on. */
  expectedRevision: number;
}

/** Result of comparing the stored source fingerprint against the disk file. */
export interface ExternalChange {
  changed: boolean;
  exists: boolean;
  disk: FileFingerprint | null;
  stored: FileFingerprint | null;
}

/** Fuzzy clustering method (F24). */
export type ClusterMethod =
  | { type: "fingerprint" }
  | { type: "ngramFingerprint"; n: number }
  | { type: "levenshtein"; maxDistance: number }
  | { type: "jaroWinkler"; minSimilarity: number };

/** Normalizations applied before cluster matching (F24). */
export interface ClusterNormalization {
  caseFold?: boolean;
  trimCollapse?: boolean;
  stripPunctuation?: boolean;
  stripDiacritics?: boolean;
  sortWords?: boolean;
}

export interface ClusterSpec {
  column: number;
  method: ClusterMethod;
  normalization?: ClusterNormalization;
  scope: ExportScope;
}

export interface ClusterMember {
  value: string;
  count: number;
}

export interface ValueCluster {
  members: ClusterMember[];
  suggested: string;
  matchKey: string;
  rowsAffected: number;
}

export interface ClusterReport {
  revision: number;
  column: number;
  scannedRows: number;
  distinctValues: number;
  totalClusters: number;
  clusters: ValueCluster[];
}

/** Semantic data type (F26) — a closed set mirrored from the Rust enum. */
export type SemanticType =
  | "uuid"
  | "email"
  | "url"
  | "ipv4"
  | "ipv6"
  | "json"
  | "percentage"
  | "currency"
  | "phoneNumber"
  | "postalCode"
  | "categorical"
  | "freeText";

/** Per-column semantic detection result (F26). */
export interface ColumnSemantics {
  column: number;
  /** The badge, when a type cleared the documented threshold. */
  detected: SemanticType | null;
  /** Best-scoring candidate even when nothing cleared the threshold. */
  bestCandidate: SemanticType | null;
  confidence: number;
  matching: number;
  conflicting: number;
  nonBlank: number;
}

export interface SemanticReport {
  revision: number;
  /** True when only a leading sample was scanned (large indexed documents). */
  sampled: boolean;
  scannedRows: number;
  threshold: number;
  columns: ColumnSemantics[];
}

/** Previewable semantic quick actions (F26) — a closed, validated set. */
export type SemanticAction =
  | "normalize"
  | "percentToDecimal"
  | "extractUrlHost"
  | "extractEmailDomain";

/** What a semantic action would change, computed without mutating (F26). */
export interface SemanticActionPreview {
  affected: number;
  /** Leading examples as [before, after] pairs. */
  examples: [string, string][];
  /** The new column's name, for the extraction actions. */
  newColumn: string | null;
}

/** One append input (F20): an open tab or a file on disk. */
export type AppendInput = { type: "openDoc"; docId: number } | { type: "file"; path: string };

/** How input columns map onto the output schema (F20). */
export type AlignMode =
  | { type: "exactName" }
  | { type: "caseInsensitiveName" }
  | { type: "position" }
  | { type: "manual"; outputHeaders: string[]; perInput: (number | null)[][] };

/** Which columns the output schema contains (F20). */
export type SchemaMode = "union" | "intersection" | "primary";

export interface AppendOptions {
  align: AlignMode;
  schema: SchemaMode;
  addSourceFile?: boolean;
  addSourceRow?: boolean;
  allowDuplicateHeaders?: boolean;
  continueOnError?: boolean;
}

export interface InputPreview {
  name: string;
  columns: number;
  mapped: number;
  /** Output columns this input cannot fill (blank in its rows). */
  missing: string[];
  warning: string | null;
}

/** Preview of an append, computed without creating anything (F20). */
export interface AppendPreview {
  outputColumns: string[];
  projectedRows: number;
  rowsEstimated: boolean;
  /** Whether the output will likely open indexed (read-only). */
  projectedIndexed: boolean;
  perInput: InputPreview[];
}

export interface InputOutcome {
  name: string;
  rows: number;
  error: string | null;
}

/** Per-input outcome report of a finished append (F20). */
export interface AppendReport {
  outputColumns: string[];
  totalRows: number;
  indexed: boolean;
  inputs: InputOutcome[];
}

/** Appended rows landed in a followed document (F19). */
export interface FollowUpdate {
  docId: number;
  newRows: number;
  totalRows: number;
  revision: number;
}

/** Why a follow watcher paused itself (F19). */
export type FollowAlertKind = "truncatedOrRotated" | "widthChanged" | "encodingChanged" | "missing";

export interface FollowAlert {
  docId: number;
  kind: FollowAlertKind;
}

/** The full CSV dialect (F18) — a closed set of validated options. */
export interface CsvDialectOptions {
  delimiter: string;
  /** null disables quoting entirely. */
  quoteCharacter: string | null;
  doubleQuote?: boolean;
  escapeCharacter?: string | null;
  commentPrefix?: string | null;
  skipLeadingRecords?: number;
  skipTrailingRecords?: number;
  /** Which post-skip record holds the headers (null = no header row). */
  headerRowIndex?: number | null;
  headerRowCount?: number;
  headerJoiner?: string;
  nullTokens?: string[];
  encoding?: string | null;
}

/** Bounded dialect preview (F18). */
export interface DialectPreview {
  sample: string[][];
  /** 1-based ORIGINAL record numbers for the sampled rows. */
  originalNumbers: number[];
  headers: string[] | null;
  duplicateHeaders: string[];
  totalRows: number;
  nCols: number;
  nullTokenCells: number;
  encoding: string;
  effective: CsvDialectOptions;
}

/** One cell's before/after in a change summary (F15). */
export interface CellChange {
  row: number;
  col: number;
  old: string;
  new: string;
}

/** One unsaved operation, summarised for the Changes panel (F15). */
export interface ChangeSummary {
  /** Stable id, valid while the operation stays on the undo stack. */
  id: number;
  epochSecs: number;
  kind: string;
  cellsAffected: number;
  sample: CellChange[];
  structural: boolean;
  revertible: boolean;
  blockedReason: string | null;
}

/** Deterministic PII detector (F28) — a closed set plus user regexes. */
export type PiiDetector =
  | { type: "email" }
  | { type: "phoneNumber" }
  | { type: "ipAddress" }
  | { type: "ssn" }
  | { type: "paymentCard" }
  | { type: "custom"; name: string; pattern: string };

export interface PiiSpec {
  detectors: PiiDetector[];
  scope: ExportScope;
}

/** One (detector, column) finding with MASKED samples only (F28). */
export interface PiiFinding {
  detector: number;
  detectorLabel: string;
  validation: string;
  column: number;
  count: number;
  samples: string[];
}

export interface PiiReport {
  revision: number;
  scannedRows: number;
  totalMatches: number;
  findings: PiiFinding[];
}

/** Redaction actions (F28) — previewed, one undo step each. */
export type RedactionAction =
  | { type: "fixedReplacement"; replacement: string }
  | { type: "keepLast"; n: number }
  | { type: "fullMask" }
  | { type: "pseudonymize"; secret: string; salt?: string | null }
  | { type: "removeColumn" }
  | { type: "removeRows" };

export interface RedactionPreview {
  revision: number;
  cellsAffected: number;
  rowsRemoved: number;
  columnRemoved: boolean;
  /** [masked before, after] pairs. */
  examples: [string, string][];
  /** The salt used for pseudonymization — reuse it for stable pseudonyms. */
  salt: string | null;
}

/** A sort key referencing its column by NAME (F25 recipes). */
export interface NamedSortKey {
  column: string;
  descending?: boolean;
}

/** The closed recipe step set (F25) — every step maps to an existing engine. */
export type RecipeStep =
  | {
      type: "reparse";
      delimiter: string | null;
      encoding: string | null;
      hasHeaderRow: boolean | null;
    }
  | { type: "validateProfile"; profileId: string; failOnIssues?: boolean }
  | { type: "filter"; spec: FilterGroup }
  | { type: "transform"; spec: TransformSpec; columns?: string[] }
  | { type: "deduplicate"; spec: DedupSpec; keep: DuplicateKeepStrategy }
  | { type: "selectColumns"; columns: string[] }
  | { type: "sort"; keys: NamedSortKey[] }
  | { type: "export"; options: ExportOptions };

export interface Recipe {
  version: number;
  name: string;
  steps: RecipeStep[];
}

export interface BatchOptions {
  recipe: Recipe;
  files: string[];
  outputDir: string;
  /** Tokens: {name} = input stem, {ext} = extension. */
  filenameTemplate: string;
  overwrite?: boolean;
  continueOnError?: boolean;
  dryRun?: boolean;
  concurrency?: number;
}

export interface FileOutcome {
  input: string;
  output: string | null;
  status: "ok" | "skipped" | "failed";
  rowsIn: number;
  rowsOut: number;
  issues: number;
  stepsApplied: number;
  error: string | null;
}

/** Structured batch result: one entry per input file (F25). */
export interface BatchReport {
  recipeName: string;
  dryRun: boolean;
  ok: number;
  skipped: number;
  failed: number;
  outcomes: FileOutcome[];
}

/** Pivot cell aggregation (F23). */
export type PivotAgg =
  | "none"
  | "count"
  | "countNonBlank"
  | "sum"
  | "mean"
  | "median"
  | "min"
  | "max"
  | "first"
  | "last";

/** The three reshape operations (F23). */
export type ReshapeSpec =
  | {
      type: "unpivot";
      idColumns: number[];
      valueColumns: number[];
      attributeName: string;
      valueName: string;
      omitBlanks?: boolean;
      addSourceRow?: boolean;
    }
  | {
      type: "pivot";
      rowKeys: number[];
      headerColumn: number;
      valueColumn: number;
      aggregation: PivotAgg;
      maxColumns?: number;
    }
  | { type: "transpose"; maxColumns?: number };

/** Preview of a reshape (F23). */
export interface ReshapePreview {
  outputColumns: number;
  projectedRows: number;
  columnSample: string[];
  duplicateCoordinates: number;
  blanksOmitted: number;
  overColumnLimit: boolean;
}

/** The closed aggregate set (F22). */
export type Aggregate =
  | "count"
  | "countNonBlank"
  | "countDistinct"
  | "sum"
  | "mean"
  | "min"
  | "max"
  | "median"
  | "first"
  | "last"
  | "concat"
  | "concatDistinct";

export interface AggregateSpec {
  aggregate: Aggregate;
  /** The aggregated column (ignored for "count"). */
  column?: number | null;
  /** Custom output column name (defaults to "agg(column)"). */
  outputName?: string | null;
}

export interface GroupBySpec {
  groupColumns: number[];
  aggregates: AggregateSpec[];
  scope: ExportScope;
  /** Case-insensitive, trimmed grouping (first-seen raw value displays). */
  normalizedGrouping?: boolean;
  blankKeys: "keep" | "exclude";
  ordering: "byKey" | "byCountDesc" | "firstSeen";
  concatSeparator?: string;
  concatMaxLen?: number;
}

/** Preview of a group-by (F22). */
export interface GroupByPreview {
  outputColumns: string[];
  groupCount: number;
  scannedRows: number;
  invalidNumeric: number;
  blankKeyRows: number;
  sample: string[][];
}

/** The classic six join types (F21). */
export type JoinType = "inner" | "left" | "right" | "full" | "leftAnti" | "rightAnti";

/** Join key normalizations (mirrors the F09 comparison options) (F21). */
export interface JoinNormalization {
  trim?: boolean;
  caseInsensitive?: boolean;
  /** Blank keys match blanks. Off = SQL NULL semantics. */
  blankEqual?: boolean;
  numericEqual?: boolean;
  dateEqual?: boolean;
}

export interface JoinSpec {
  join: JoinType;
  /** Ordered composite key columns, one list per side (equal lengths). */
  leftKeys: number[];
  rightKeys: number[];
  /** Right-side columns to include in the output. */
  rightColumns: number[];
  /** Lookup mode: right-side keys must be unique. */
  lookup?: boolean;
  collisionSuffix?: string;
  normalization?: JoinNormalization;
  /** Refuse to run when projected rows exceed this (confirm = raise it). */
  maxOutputRows?: number | null;
}

/** Cardinality preview of a join (F21). */
export interface JoinPreview {
  outputColumns: string[];
  matchedPairs: number;
  leftRows: number;
  rightRows: number;
  leftUnmatched: number;
  rightUnmatched: number;
  leftDuplicateKeys: number;
  rightDuplicateKeys: number;
  projectedRows: number;
  expands: boolean;
  lookupConflict: boolean;
}

/** Outlier detection method (F30) — a closed, validated set. */
export type OutlierMethod =
  | { type: "iqr"; k: number }
  | { type: "mad"; threshold: number }
  | { type: "zScore"; threshold: number }
  | { type: "percentile"; lower: number; upper: number }
  | { type: "rareCategory"; maxShare: number }
  | { type: "unexpectedCategory"; allowed: string[] }
  | { type: "patternMismatch"; pattern: string };

export interface OutlierSpec {
  column: number;
  method: OutlierMethod;
  /** Group-wise analysis: statistics computed per group key. */
  groupColumns: number[];
  scope: ExportScope;
}

/** Corrective actions (F30) — all previewed, all one undo step. */
export type OutlierAction = "replaceBlank" | "replaceMedian" | "capToBounds" | "removeRows";

export interface GroupSummary {
  key: string[];
  count: number;
  flagged: number;
  mean: number | null;
  median: number | null;
  stdDev: number | null;
  q1: number | null;
  q3: number | null;
  mad: number | null;
  lower: number | null;
  upper: number | null;
}

export interface FlaggedValue {
  /** Absolute row index. */
  row: number;
  value: string;
  group: string[];
  reason: string;
}

export interface OutlierReport {
  revision: number;
  scannedRows: number;
  considered: number;
  flagged: number;
  blanks: number;
  invalidNumeric: number;
  groups: GroupSummary[];
  groupsTotal: number;
  sample: FlaggedValue[];
}

export interface OutlierActionPreview {
  revision: number;
  cellsAffected: number;
  rowsRemoved: number;
  examples: { row: number; before: string; after: string }[];
}

/** Missing-value repair operation (F29) — a closed, validated set. */
export type RepairOp =
  | { type: "normalizeNullTokens"; tokens: string[] }
  | { type: "fillConstant"; value: string }
  | { type: "fillForward"; groupColumns: number[] }
  | { type: "fillBackward"; groupColumns: number[] }
  | { type: "fillMean" }
  | { type: "fillMedian" }
  | { type: "fillMode" }
  | { type: "interpolate"; extrapolate?: boolean }
  | { type: "removeRows"; threshold: number }
  | { type: "removeColumns"; threshold: number };

export interface RepairSpec {
  op: RepairOp;
  /** Target columns (the cells examined and repaired). */
  columns: number[];
  /** Which rows participate; rows outside are never modified. */
  scope: ExportScope;
}

export interface RepairExample {
  row: number;
  col: number;
  before: string;
  after: string;
}

/** What a repair would do, computed without mutating (F29). */
export interface RepairPreview {
  revision: number;
  cellsAffected: number;
  rowsRemoved: number;
  columnsRemoved: number;
  /** [column, computed fill value] for the statistical fills. */
  fillValues: [number, string][];
  /** Non-blank cells the statistics had to ignore as non-numeric. */
  invalidNumeric: number;
  examples: RepairExample[];
}

/** Numeric comparison operator for cross-column rules (F27). */
export type CompareOp = "lt" | "le" | "gt" | "ge" | "eq" | "ne";

/** Condition on the "when" column of a conditional-required rule (F27). */
export type WhenCondition =
  | { type: "equals"; value: string }
  | { type: "nonBlank" }
  | { type: "blank" };

/** Cross-column validation rule (F27) — a closed set; columns by NAME. */
export type CrossRule =
  | { type: "columnsEqual"; left: string; right: string; negate?: boolean }
  | { type: "numericCompare"; left: string; op: CompareOp; right: string }
  | { type: "dateOrder"; earlier: string; later: string; allowEqual?: boolean }
  | { type: "conditionalRequired"; whenColumn: string; when: WhenCondition; thenRequired: string }
  | { type: "exactlyOne"; columns: string[] }
  | { type: "atLeastOne"; columns: string[] }
  | { type: "atMostOne"; columns: string[] }
  | {
      type: "sumEquals";
      parts: string[];
      total: string;
      tolerance: number;
      tolerancePercent?: boolean;
    }
  | { type: "allowedCombinations"; columns: string[]; allowed: string[][] };

/** One sampled cross-validation violation (F27). */
export interface CrossViolation {
  /** Absolute row index. */
  row: number;
  /** [column name, value] for the rule's referenced columns. */
  values: [string, string][];
  reason: string;
}

/** Per-rule cross-validation outcome (F27). */
export interface RuleViolations {
  /** Index into the submitted rule list. */
  rule: number;
  description: string;
  violations: number;
  /** First violations (bounded sample). */
  sample: CrossViolation[];
}

export interface CrossValReport {
  revision: number;
  scannedRows: number;
  /** Sum across rules (a row can violate several). */
  totalViolations: number;
  /** Distinct rows violating at least one rule. */
  violatingRows: number;
  rules: RuleViolations[];
}

/** Copy As output format (F14). */
export type CopyFormat =
  | { type: "tsv" }
  | { type: "csvCurrent" }
  | { type: "csvCustom"; delimiter: string; quoteStyle: string; lineEnding: string }
  | { type: "jsonObjects" }
  | { type: "jsonArrays" }
  | { type: "jsonLines" }
  | { type: "markdown" }
  | { type: "sqlValues" };

/** Paste Special options (F14) — a closed, validated set. */
export interface PasteSpecialOptions {
  mode: "overwrite" | "insertRows";
  transpose?: boolean;
  skipBlanks?: boolean;
  trim?: boolean;
  repeatToFill?: boolean;
  firstRowHeaders?: boolean;
}

/** What a Paste Special preview reports before anything mutates (F14). */
export interface PastePreview {
  rows: number;
  cols: number;
  targetRow: number;
  targetCol: number;
  addedRows: number;
  addedCols: number;
  headerChanges: string[];
  sample: string[][];
  warnings: string[];
}

export interface RowsResponse {
  start: number;
  rows: string[][];
  dirty: boolean[][];
}

export interface OpenOptions {
  delimiter?: string;
  encoding?: string;
  hasHeaderRow?: boolean;
  /**
   * Open fully in memory even when the size estimate recommends asking (the
   * "Open editable" choice in the F10 open-mode dialog).
   */
  forceInMemory?: boolean;
}

/** What `probe_open` reports so the UI can offer indexed mode (F10). */
export interface OpenEstimate {
  fileSize: number;
  estimatedRows: number;
  estimatedMemory: number;
  needsDecision: boolean;
  encoding: string;
}

/** Handles returned by `start_open_indexed`. */
export interface IndexedOpenStart {
  jobId: number;
  docId: number;
}

export interface SortKey {
  column: number;
  descending: boolean;
}

export interface CellRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface FindOptions {
  query: string;
  regex?: boolean;
  caseSensitive?: boolean;
  wholeCell?: boolean;
  selection?: CellRect;
  /** Stop after this many matches (used for indexed documents). */
  limit?: number;
}

export interface FindMatch {
  row: number;
  col: number;
}

export interface ReplaceResult {
  replaced: number;
  meta: DocumentMeta;
}

export interface SelectionStats {
  count: number;
  numericCount: number;
  sum: number;
  avg: number | null;
  min: number | null;
  max: number | null;
}

export type ColumnKind = "number" | "date" | "bool" | "text";

export interface NumericSummary {
  min: number;
  max: number;
  mean: number;
}

export interface ColumnSummary {
  /**
   * True when the statistics cover only a leading sample of the rows
   * (indexed documents over the sample limit), not the whole document.
   */
  sampled: boolean;
  column: number;
  kind: ColumnKind;
  count: number;
  nulls: number;
  unique: number;
  numeric: NumericSummary | null;
}

export type FilterOp =
  | "equals"
  | "notEquals"
  | "contains"
  | "notContains"
  | "startsWith"
  | "endsWith"
  | "gt"
  | "gte"
  | "lt"
  | "lte"
  | "isEmpty"
  | "notEmpty"
  | "regex";

export type Conjunction = "and" | "or";

export interface FilterCondition {
  type: "condition";
  /** Stable client-side id for React keys (ignored by the backend). */
  id: string;
  column: number;
  op: FilterOp;
  value: string;
  caseSensitive: boolean;
}

export interface FilterGroup {
  type: "group";
  /** Stable client-side id for React keys (ignored by the backend). */
  id: string;
  conjunction: Conjunction;
  nodes: FilterNode[];
}

export type FilterNode = FilterCondition | FilterGroup;

/** What to do with the previous destination file when saving over it. */
export type BackupPolicy = "none" | "single";

export interface ExportOptions {
  delimiter: string;
  encoding: string;
  quoteStyle: QuoteStyle;
  lineEnding: LineEnding;
  bom: boolean;
  includeHeaders: boolean;
  /** Backup policy for the previous destination file (default "none"). */
  backup?: BackupPolicy;
}

/**
 * Which slice of the document an export writes (F04). Row/rect coordinates
 * are display-space (what the user sees under the active filter).
 */
export type ExportScope =
  | { type: "all" }
  | { type: "visibleRows" }
  | { type: "selectedRows"; rows: number[] }
  | { type: "selectedColumns"; columns: number[] }
  | { type: "selectedRange"; rect: CellRect };

/** How to split an export across multiple output files (F04). */
export type SplitOptions =
  | { type: "none" }
  | { type: "maxRows"; rowsPerFile: number }
  | { type: "approximateBytes"; maxBytes: number }
  | { type: "groupByColumn"; column: number };

/** Expected shape of a scoped export, shown before writing anything. */
export interface ScopeCounts {
  rows: number;
  cols: number;
}

/** The supported cleanup operations (F06). Closed set: never user code. */
export type TransformSpec =
  | { type: "trim" }
  | { type: "collapseWhitespace" }
  | { type: "uppercase" }
  | { type: "lowercase" }
  | { type: "titleCase" }
  | { type: "replaceText"; find: string; replace: string; caseSensitive: boolean }
  | { type: "replaceRegex"; pattern: string; replace: string }
  | { type: "fillBlank"; value: string }
  | { type: "normalizeBooleans"; trueValue: string; falseValue: string }
  | { type: "normalizeDates"; format: string }
  | { type: "normalizeNumbers"; decimalComma: boolean }
  | { type: "addPrefix"; prefix: string }
  | { type: "addSuffix"; suffix: string }
  | { type: "splitByDelimiter"; column: number; delimiter: string }
  | { type: "splitByRegex"; column: number; pattern: string }
  | { type: "mergeColumns"; columns: number[]; separator: string };

export type TransformErrorPolicy = "failAll" | "skipInvalid";

export interface TransformExample {
  row: number;
  col: number;
  before: string;
  after: string;
}

/** Preview of a transform's full effect; nothing has been mutated. */
export interface TransformPreview {
  affectedCells: number;
  parseFailures: number;
  examples: TransformExample[];
  failureExamples: TransformExample[];
  columnsInserted: string[];
  columnsRemoved: string[];
  /** True when the values of every row change regardless of row scope. */
  appliesToAllRows: boolean;
  /** Echo back to applyTransform; rejected when the document moved on. */
  expectedRevision: number;
}

/** How two documents are compared (F09). */
export type CompareMode = "positional" | "keyed";

export interface CompareSpec {
  mode: CompareMode;
  /** LEFT columns forming the row key (keyed mode). */
  keyColumns: number[];
  /** (left column, right column) pairs; empty = identity by position. */
  columnMapping: [number, number][];
  trim: boolean;
  caseInsensitive: boolean;
  blankEqual: boolean;
  numericEqual: boolean;
  dateEqual: boolean;
}

export type DiffStatus = "added" | "removed" | "changed" | "unchanged" | "conflict";

export interface CellDifference {
  leftCol: number;
  rightCol: number;
  left: string;
  right: string;
}

export interface DiffRecord {
  status: DiffStatus;
  key: string[];
  leftRow: number | null;
  rightRow: number | null;
  cells: CellDifference[];
}

export interface CompareSummary {
  added: number;
  removed: number;
  changed: number;
  unchanged: number;
  conflicts: number;
  total: number;
}

export interface CompareInfo {
  compareId: number;
  leftDoc: number;
  rightDoc: number;
  leftRevision: number;
  rightRevision: number;
  summary: CompareSummary;
}

export interface ComparePage {
  records: DiffRecord[];
  totalFiltered: number;
}

/** Key definition + normalization options for duplicate detection (F07). */
export interface DedupSpec {
  keyColumns: number[];
  trim: boolean;
  caseInsensitive: boolean;
  collapseWhitespace: boolean;
  /** Whether rows whose COMPLETE key is blank group with each other. */
  blankKeysEqual: boolean;
  /** Drop rows whose complete key is blank from consideration entirely. */
  excludeBlankKeys: boolean;
}

export type DuplicateKeepStrategy = "first" | "last" | "mostComplete";

export interface DuplicateGroup {
  /** Normalized key values, for display. */
  key: string[];
  /** Absolute row indices, in source order (possibly truncated). */
  rows: number[];
  /** Exact size of the group. */
  size: number;
}

export interface DuplicateReport {
  /** Document revision this report was computed against. */
  revision: number;
  consideredRows: number;
  groupCount: number;
  /** Excess rows: what "remove duplicates" would delete. */
  duplicateRows: number;
  remainingRows: number;
  sampleGroups: DuplicateGroup[];
}

/** Which rows a column profile covers (F05). */
export type ProfileScope = "all" | "visibleRows";

export interface ValueCount {
  value: string;
  count: number;
}

export interface TypeCounts {
  number: number;
  date: number;
  bool: number;
  text: number;
}

export interface NumericProfile {
  min: number;
  max: number;
  mean: number;
  median: number;
  q1: number;
  q3: number;
}

export interface TextProfile {
  minLen: number;
  maxLen: number;
  avgLen: number;
}

/** Interactive profile of one column (F05). */
export interface ColumnProfile {
  column: number;
  scope: ProfileScope;
  /** Document revision this profile was computed against. */
  revision: number;
  rowCount: number;
  blankCount: number;
  inferredKind: ColumnKind;
  typeCounts: TypeCounts;
  distinctCount: number;
  /** True above the documented exact limit (HyperLogLog estimate). */
  distinctIsApproximate: boolean;
  topValues: ValueCount[];
  /** True when the bounded sketch evicted counters (counts are lower bounds). */
  topIsApproximate: boolean;
  numeric: NumericProfile | null;
  earliestDate: string | null;
  latestDate: string | null;
  text: TextProfile | null;
}

/** How a file profile decides whether it applies to a path (F08). */
export type ProfileMatch =
  | { type: "exactPath"; path: string }
  | { type: "directory"; directory: string }
  | { type: "extension"; extension: string }
  | { type: "glob"; pattern: string };

export type ExpectedType = "number" | "date" | "bool" | "text";

export interface RegexRule {
  column: string;
  pattern: string;
}

export interface RangeRule {
  column: string;
  min: number | null;
  max: number | null;
}

/** A reusable description of a recurring file format (F08). */
export interface FileProfile {
  id: string;
  name: string;
  matcher: ProfileMatch;
  /** Auto-reparse matching (clean) documents with these settings. */
  autoApply: boolean;
  delimiter: string | null;
  encoding: string | null;
  hasHeaderRow: boolean | null;
  defaultExport: ExportOptions | null;
  expectedColumns: string[];
  enforceOrder: boolean;
  expectedTypes: [string, ExpectedType][];
  requiredColumns: string[];
  uniqueColumns: string[];
  regexRules: RegexRule[];
  rangeRules: RangeRule[];
  /**
   * F26: user overrides of detected semantic types, keyed by column NAME so
   * they survive rescans. "freeText" forces plain text.
   */
  semanticTypes: [string, SemanticType][];
  /** F27: cross-column validation rules (closed DTO set, columns by name). */
  crossRules: CrossRule[];
  /** F12: named views saved for matching files. */
  namedViews?: NamedView[];
  /** F12: the view last applied to a matching file, restored on reopen. */
  lastViewId?: string | null;
}

/** One key of a named view's non-destructive sort (F12), by stable column ID. */
export interface ViewSortKey {
  columnId: string;
  descending?: boolean;
}

/**
 * A named, reusable, NON-DESTRUCTIVE way of looking at a matching document
 * (F12): row filter + view sort + column layout. Columns are referenced by
 * stable logical IDs (`DocumentMeta.columnIds`); the filter keeps its column
 * indices but carries the ID snapshot it was saved against so it can be
 * remapped (or warn recoverably) after structural edits. Applying a view
 * never mutates data and never marks a document dirty.
 */
export interface NamedView {
  id: string;
  name: string;
  filter: FilterGroup | null;
  /** Column IDs at save time, aligned with the filter's column indices. */
  filterColumnIds: string[];
  sortKeys: ViewSortKey[];
  hiddenColumnIds: string[];
  /** Arbitrary pinned columns (not just a leading count), in pin order. */
  pinnedColumnIds: string[];
  /** Display order for unpinned columns; IDs not listed keep file order. */
  columnOrder: string[];
  /** Column widths in px, keyed by column ID. */
  columnWidths: Record<string, number>;
  wrapText: boolean;
}

/** The persisted settings document (versioned JSON in app-data). */
export interface AppSettings {
  version: number;
  profiles: FileProfile[];
  /**
   * F11: shortcut overrides keyed by stable command id, in normalized
   * `mod+shift+k` syntax. `null` unbinds; a missing key keeps the default.
   */
  shortcutOverrides?: Record<string, string | null>;
  /** F16: opt-in crash-recovery journaling (privacy disclosure applies). */
  recoveryEnabled?: boolean;
  /** F16: journals older than this many days are swept at startup. */
  recoveryRetentionDays?: number;
}

/** One recoverable session found at startup (F16). */
export interface RecoverableSession {
  journalPath: string;
  sourcePath: string;
  fileName: string;
  lastEditEpochSecs: number;
  operationCount: number;
  /** Source changed since journaling — blind replay blocked. */
  sourceChanged: boolean;
  sourceMissing: boolean;
  /** Journal version mismatch — kept for manual recovery only. */
  incompatible: boolean;
}

/** One violated profile rule. */
export interface ProfileIssue {
  kind: string;
  column: string | null;
  detail: string;
  affectedCount: number;
}

/** Outcome of checking a document against a profile. */
export interface ProfileValidation {
  profileId: string;
  ok: boolean;
  issues: ProfileIssue[];
}

/** One cell (or header) that a target encoding cannot represent. */
export interface EncodingIncompatibility {
  /** Data-row index; null for a header cell. */
  row: number | null;
  col: number;
  value: string;
}

/** Result of scanning for characters a target encoding cannot represent. */
export interface EncodingCompatibility {
  encoding: string;
  compatible: boolean;
  affectedCells: number;
  /** First affected locations (capped at 100). */
  samples: EncodingIncompatibility[];
}
