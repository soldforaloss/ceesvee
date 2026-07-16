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
}

export type DocumentBacking = "editable" | "indexedReadOnly";

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
}

/** The persisted settings document (versioned JSON in app-data). */
export interface AppSettings {
  version: number;
  profiles: FileProfile[];
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
