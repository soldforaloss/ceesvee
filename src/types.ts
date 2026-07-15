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

export interface RowsResponse {
  start: number;
  rows: string[][];
  dirty: boolean[][];
}

export interface OpenOptions {
  delimiter?: string;
  encoding?: string;
  hasHeaderRow?: boolean;
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
