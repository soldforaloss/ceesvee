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
  hasHeaderRow: boolean;
  delimiter: string;
  encoding: string;
  hadBom: boolean;
  lineEnding: LineEnding;
  dirty: boolean;
  canUndo: boolean;
  canRedo: boolean;
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
