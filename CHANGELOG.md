# Changelog

All notable changes to CEESVEE are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Parquet & Arrow interoperability** (palette → "Open Parquet/Arrow…" and
  "Export as Parquet/Arrow…"): open and export typed columnar datasets —
  Apache Parquet, Arrow IPC files (Feather v2 is the Arrow IPC file format),
  and Arrow IPC streams — preserving types and nulls. Opening a `.parquet` /
  `.arrow` / `.feather` / `.ipc` file (drag-and-drop, "Open file…", or the
  command) first shows an inspect dialog: container format, row and
  row-group/batch counts, compression codec, the schema mapped to the F31
  logical types (nested fields indented, timezones shown), and an
  editable-memory estimate — before anything is loaded. Choose read-only
  (an indexed columnar backing with windowed reads over row groups / record
  batches and a bounded decoded-block cache, so the grid, filters, and
  export work with bounded memory on multi-gigabyte files) or convert to
  editable behind an explicit memory check. Signed and unsigned 64-bit
  integers (so `u64::MAX` round-trips losslessly), exact decimal
  precision+scale, floats, booleans, dates, timestamps with their time-zone
  metadata, and UTF-8 strings all survive intact, and a NULL stays distinct
  from an empty string end to end (editable opens preserve the distinction
  through collision-free per-column null tokens). Structs flatten to stable
  path-based column names; each list/map/struct field takes an explicit
  per-field policy (keep as JSON, explode into rows on an editable open, or
  drop). Equality and range filters on numeric/date columns of indexed
  parquet documents skip whole row groups using their statistics, with
  results identical to a full scan. Export any scope (all rows, the filtered
  view, selected rows / columns / range) to Parquet (uncompressed, Snappy,
  or Zstd, with a configurable row-group size), an Arrow IPC file, or an
  Arrow IPC stream, as a cancellable job through the atomic-save pipeline;
  typed export maps each column's declared logical type to the matching
  arrow type (Int64/UInt64, Decimal128 with unified precision/scale, Date32,
  microsecond or nanosecond timestamps carrying the schema's time zone),
  while null tokens and columnar NULLs export as real nulls distinct from
  empty strings. Cells that cannot be represented under the declared types
  are written as NULL and counted into a per-column warning report; columns
  without a schema export as text verbatim. A columnar document opens
  unsaved so a later Save can never overwrite the binary source with CSV.
- **Row bookmarks, tags & notes** (F40): mark and annotate records without
  touching the source data. Star or flag a row, apply multiple named tags
  (a per-document tag namespace with usage counts), and attach a row note or
  per-column cell notes with an optional author label and created/updated
  timestamps. Annotations are pinned by row identity — a user-selected
  composite key (survives reordering; duplicate keys are reported ambiguous)
  or, otherwise, the source record number plus a content fingerprint — and are
  re-matched on reparse or external change into a matched / ambiguous /
  orphaned review list, so a note is never silently attached to an uncertain
  row (a deleted row keeps its annotation as an orphan until the delete is
  committed or reverted). List every annotation in a side panel with
  jump-to-row and a review queue for the ambiguous and orphaned ones; filter
  the grid by annotation state (starred, flagged, tagged, has-note) through
  the existing filter view; copy a tag into a real column as one previewed,
  undoable operation; and export the annotations to JSON or CSV on an
  explicit action. Annotations are stored in
  the active project's workspace file or, with no project open, a versioned
  `<file>.ceesvee-notes.json` sidecar written atomically — never inside the
  CSV, and never in an ordinary data export.
- **Record form view** (palette → "Toggle record form", or `Ctrl+Shift+R`):
  a dockable single-record editor for very wide / record-oriented tables.
  Each field shows its schema-aware label (the data-dictionary display name
  and description when documented), type and semantic badges, an autosizing
  editor for long or multiline text, a raw-vs-formatted toggle (raw and
  formatted never disagree about the stored value), and — only where the
  schema declares null tokens — a null-token-vs-blank control. Per-field
  validation surfaces three things distinctly: a strict violation that blocks
  the save, an advisory violation that only warns, and any advisory issue
  already recorded for the cell (closing a deliberate gap in the typed-edit
  path). A changed-field indicator, copy-value, and jump-to-grid-column sit on
  every field. Navigate previous / next / go-to across the _visible_ records,
  so the form respects the active filter and view sort and always edits the
  correct absolute row; a draft commits every changed field as one undo step,
  a strict-invalid draft cannot commit, and moving away from an unsaved draft
  prompts to save or discard — or auto-saves, per a persisted preference.
  Fields can be arranged into named groups, hidden, and shown compact or
  comfortable (persisted per document); indexed documents open the form
  read-only. The form reuses the F40 annotations: a row bookmark strip in the
  header (star, flag, tags, and a row note) and a per-field cell-note indicator
  that opens the same note editor — annotations taken in the form resolve to
  the same rows as the grid gutter and the annotations panel, and stay
  available on a read-only form since they are pure metadata.
- **Conditional highlighting** (view-only decoration; rules persist in named
  views and file profiles): flag cells and rows with prioritized rules whose
  conditions cover equals / not-equals, contains, regular expression, numeric
  and date ranges, blank / null / invalid (schema-aware), duplicate values,
  diagnostic issues, cross-column violations, statistical outliers, and
  changed-since-save — plus bookmarked / flagged / tagged, backed by the F40
  row annotations (only unambiguously matched rows are decorated). Each rule
  targets the matched cell, its whole row, or selected columns, and carries a
  theme-aware semantic decoration (accent / info / warn / error / success /
  neutral tone, subtle / normal / strong emphasis, optional icon and text
  style) rather than a raw colour, so light and dark stay readable. Overlaps
  resolve by priority (ties break by rule id) into one winning decoration per
  cell, flattened server-side so the grid receives only the visible window — a
  million-row scroll stays smooth with no per-cell IPC. Highlighting never
  touches data: no dirty flag, no undo entry, exports unchanged. Per-rule match
  sets are cached and a rule edit invalidates only its own cache; the
  annotation-backed rules re-resolve only when an annotation (or the data)
  changes; an "explain" query lists every rule matching a cell in priority
  order; and a match report exports to JSON or CSV as a cancellable, atomic
  job.
- **JSON & JSON Lines interoperability** (palette → "Open JSON…" and
  "Export as JSON…"): open structured JSON without pre-converting to CSV —
  an array of objects, an array of arrays, JSON Lines / NDJSON, or an
  object holding a record array at a JSON Pointer (candidate paths are
  auto-detected, or type your own). The import preview reports the detected
  shape, the inferred per-column type, projected rows and columns,
  nested-object and array-valued fields, and exact present / explicit-null /
  missing counts — and a MISSING property stays distinct from an explicit
  `null` end to end, through editing and back out on export. Choose how
  nested objects map (flatten to dotted-path columns, preserve as compact
  JSON, or ignore selected paths) and how arrays map (preserve as JSON, join
  primitives with a separator, explode into one row per element, or reject);
  exploding two array fields at once requires an explicit cartesian-or-zip
  choice. JSON Lines opens with bounded memory and can back a read-only
  indexed document like a large CSV, and the column union across records is
  deterministic (first-seen order, then alphabetical for keys that appear
  later). Export any scope the CSV exporter supports as an array of objects,
  an array of arrays, or JSON Lines: columns with a declared schema emit real
  JSON numbers, booleans, and re-inflated JSON (null tokens become `null`),
  nested objects rebuild from dotted-path columns, and duplicate or
  conflicting output paths are rejected before a byte is written. Invalid
  JSON is reported with its byte offset, line, column, and surrounding
  context and never leaves a partially opened document; every import and
  export runs as a cancellable job with progress, and imported files land
  through the same pipeline as a CSV open (tabs, dirty tracking,
  diagnostics). Exported JSON is always UTF-8 and reparses cleanly.
- **Project workspaces** (Projects toolbar menu, or palette → "New
  project", "Open project…"): save a working context across related
  datasets in a versioned `.ceesveeproj` JSON file. Saving captures the
  live session — referenced source documents (with file fingerprints and
  parse settings), open-tab order and active tab, panel layout, and each
  document's named views and active view. The versioned section store also
  round-trips file profiles, schemas, recipes, join mappings, comparison
  definitions, and row-key definitions when a template or existing project
  file carries them (typed, registered sections that future features
  extend; with reserved sections for annotations, data dictionaries, and
  saved queries) — capturing those sections from a live session is planned
  for a later release. Projects reference data, never embed it: every save
  and section write structurally rejects cell-value payloads. Source paths
  are stored relative to the project file when possible (absolute across
  drive roots), saves are atomic, and unknown fields written by newer
  versions survive a round-trip while newer major format versions are
  rejected with a clear error. Opening a project surfaces missing, moved,
  changed, and column-incompatible sources with per-source choices (locate
  a replacement, open available only, remove, or cancel), reapplies each
  opened document's saved views only after fingerprint and
  column-compatibility checks (warn, never break), and never auto-runs
  recipes, queries, joins, or exports.
  Templates capture the configuration without source paths to initialize a
  repeatable workflow. A project bar shows the current project and a dirty
  dot; a Projects toolbar menu and command-palette entries cover New, New
  from template, Open, Save, Save As, Save as Template and Close; the open
  dialog lists each source's status with per-source actions (open, relink,
  leave out, remove, or open-available-only); and quitting or closing a
  project with unsaved workspace changes prompts to save first.
- **Data dictionary** — document what each column MEANS. Every column
  carries an optional entry (display name, description, analytical role,
  unit, source, sensitivity, allowed values, example, owner, notes) keyed
  by its stable column ID, so the documentation survives renames and
  reorders and is restored by undo/redo; deleting a column reports its
  entry as orphaned and keeps it until you explicitly discard it (an undo
  re-attaches it). Editing the dictionary is pure metadata: it has its own
  revision, like the schema, and never rewrites a cell or marks the
  document dirty. The editor prefills each column's technical name and
  inferred F31 type. Dictionaries import and export as versioned CEESVEE
  JSON, Markdown documentation, and tabular CSV documentation; an import
  merges incoming metadata by column ID (or by mapped column name when the
  IDs differ) and surfaces every field-level conflict for explicit
  resolution before it replaces anything. File profiles can require
  documentation fields (e.g. a description and owner on every column) as
  ordinary validation issues, and columns classified confidential or
  restricted are folded into the PII scan preflight even when no detector
  matches them.
- **Sampling and partitioning** (F48): carve reproducible subsets and splits
  out of a document without deleting a single row. Eight sampling methods —
  first N, last N, a random fixed count (reservoir, single-pass and bounded so
  it works over indexed sources), a random percentage, systematic every-Nth
  (with a fixed or seed-drawn offset), proportional stratified (with a reported
  tolerance), balanced (equal per stratum, shortfalls reported), and
  deterministic hash-based (a stable subset that does not move when unrelated
  rows change) — plus partitioning into weighted, named outputs
  (train/validation/test presets or custom), optionally stratified or
  group-preserving (rows sharing key-column values never split across
  partitions). Every run is driven by a seed (supplied, or crypto-generated and
  surfaced) so the same source + settings + seed produces byte-identical
  outputs; partitions are disjoint by construction. A preview shows the
  projected AND exact counts for each output (with a strata table when
  stratifying) before anything is written, outputs preserve source order or an
  explicit shuffle, results become new derived documents or direct CSV exports,
  and each export gets a JSON manifest recording the method, seed, source
  fingerprint, scope, and a SHA-256 per output. Reachable from the command
  palette as "Sample rows…" and "Partition dataset…", with a per-method
  parameter editor, a seed field (with a Regenerate button showing the value
  in use), a live projected-vs-exact count preview, cancellable progress, and
  a destination picker (new documents or a chosen export folder).
- **Explicit schemas and typed columns** (palette → "Edit schema…", or a
  column header's menu): declare an explicit logical type per column —
  text, integer, decimal, float, boolean, date, datetime, UUID, or JSON —
  as a layer ON TOP of the raw text that never rewrites a cell, so a ZIP
  column declared text keeps its leading zeroes. Every cell resolves to
  one of five distinguishable states — a missing field in a ragged short
  row, a configured null token, an empty string, a valid typed value, or a
  value invalid for the declared type — and empty strings stay distinct
  from null tokens everywhere. Schemas key columns by stable IDs so
  assignments survive renames and reorders; one click infers a schema from
  the data (with leading-zero protection so numeric-looking codes stay
  text), and whole schemas import and export as versioned JSON (unknown
  columns are skipped and reported). The searchable per-column editor sets
  the logical type, nullability, null tokens (including the empty string),
  locale, time zone, custom input formats, a display format, and an
  advisory/strict validation mode. Parsing is locale-aware (`de-DE` /
  `fr-FR` decimals, Swiss grouping, …) and timezone-aware (IANA zones, DST
  folds resolve earliest, gaps are invalid); a display format only changes
  how a cell is shown — the editor, copy, and fill always see the raw
  stored text, and changing a display format never marks the document
  dirty. Column headers show a violet badge for the declared type, and
  sorting (destructive and view), range filters, column profiles and
  summaries, group-by aggregates, join key equivalence, and cross-column
  validation all prefer the declared logical type over heuristics —
  full-width integer, exact decimal, and chronological date comparisons,
  locale-parsed decimals, and null tokens counted as null (still distinct
  from empty strings). The editor surfaces each column's five-state counts
  with invalid-value samples and runs a canonical conversion as a
  previewed, cancellable, single-undo operation that leaves invalid and
  null cells untouched. Cell edits are gated by the column's mode — strict
  rejects an invalid edit before it reaches the model (in the editor and
  the backend), advisory accepts it and records a bounded, retrievable
  issue — while schema edits themselves never touch the undo stack.

### Internal

- **Shared tabular contracts**: new backend `TabularSource` /
  `TabularSink` traits — logical schema (stable column IDs + declared
  types), row-count hints, bounded windowed reads with a missing-vs-empty
  cell distinction, cooperative cancellation, content fingerprints, and
  atomic streamed CSV output through the existing crash-safe save
  pipeline. Both document backings (editable and indexed) read through the
  contract, and the multi-file append reader now streams its inputs over
  it. The contract is explicitly scoped: text-representable values (binary
  columns are a documented follow-up), a fixed per-source schema, and
  forward/sequential window streaming so a future single-pass source need
  not fake random access. Groundwork for upcoming import/export formats and
  sampling.
- **Shared row identity**: new backend model for upcoming annotations,
  patches, and three-way merge — session-stable editor row ids, source
  record numbers for read-only documents, normalized composite keys
  (trim / case fold / Unicode NFKC, deterministic order) with explicit
  duplicate-key reporting (every involved row is flagged, never silently
  first-wins), and boundary-safe SHA-256 row content hashes.

## [0.4.0]

### Added

- **Named views and column layouts** (palette → "Named views…", or the
  status-bar chip): save reusable, NON-destructive ways of looking at a
  file — row filter, a new view-only sort that never reorders the
  source rows or touches undo, hidden columns, arbitrary pinned
  columns (not just a leading count), drag-reordering, per-column
  widths, and wrap text. Views persist in the file's profile, restore
  on reopen, and reference columns by stable IDs so renames — and
  reorders, inserts, deletes, even undo/redo — never break them;
  a missing column produces a recoverable warning instead of
  corrupting the view. Applying a view never marks the document
  dirty, edits in a sorted/filtered view land on the correct source
  cells, exports ask explicitly whether to respect hidden columns and
  view order, and plain Save always keeps the file's own row and
  column order. Auto-fit widths (one, selected, or all columns) and
  the view sort work on read-only indexed documents too.
- **Follow / tail mode** (palette → "Open in follow mode…"): watch a
  growing CSV log inside CEESVEE. The document opens read-only; a watcher
  appends complete records as the file grows, holding partial trailing
  records — including open quoted fields spanning chunks — until they
  complete. Pause/resume (nothing is lost while paused), a new-row
  counter, jump-to-newest, and a one-click "only new rows" filter;
  truncation, replacement, rotation, wider records, and encoding changes
  raise an alert (with restart-or-stop options) instead of ever silently
  combining old and new content. Closing the tab stops the watcher and
  releases the file handle.
- **Advanced dialect import** (palette → "Advanced import…"): open
  real-world "CSV" files with metadata preambles, comment lines, custom
  quote/escape characters (or quoting disabled entirely), multi-row
  headers combined with a configurable joiner, skipped trailing footers,
  and analysis-level null tokens (raw text always retained). The preview
  shows ORIGINAL record numbers so excluded preambles stay visible,
  flags duplicate combined headers (made unique deterministically on
  apply), and echoes the effective dialect. Applying reinterprets the
  file through the guarded reparse path — dirty documents require
  explicit confirmation — and saving afterwards writes only the current
  grid, never re-adding skipped preamble or comment records.
- **Crash recovery** (opt-in, palette → "Recover unsaved work…"): an
  append-only local journal records every edit operation (including
  undo/redo) the moment it happens. After a crash, power failure, or
  forced termination, startup lists recoverable sessions — file, last
  edit time, operation count, and whether the source changed — with
  Recover, Open Copy, Discard, and Show Location. Recovery replays the
  operations onto a fresh parse of the source in their original order,
  producing a DIRTY document; the source file is never written, a changed
  source fingerprint blocks blind replay (Open Copy takes over), corrupt
  trailing journal data never invalidates the complete operations before
  it, and incompatible journal versions are kept for manual recovery.
  Journals reset on every successful save (atomic compaction), delete on
  clean close, expire on a configurable retention, and carry a privacy
  disclosure (they contain edited cell values) plus a "Delete all
  recovery data" action.
- **Change inspector with selective revert** (palette → "Changes since
  save"): a side panel listing every unsaved operation — kind, time,
  affected cells with before/after values — exactly mirroring the dirty
  state (saving clears it). Revert one cell, a whole operation, all edits
  in a column, or everything since the last save; every revert is a NEW
  operation on the ordinary undo stack, so reverting is itself undoable.
  Structural operations can only be reverted whole, and earlier selective
  reverts are blocked (with the reason) once a later structural change
  depends on them — Revert all stays available and is one undo step.
  Before/after values copy to the clipboard, samples jump to their cell,
  and the whole list exports as a JSON change report.
- **PII detection and redaction** (palette → "Find personal data…"):
  deterministic detectors — emails, phone numbers, IP addresses, US SSN
  patterns, Luhn-validated payment-card candidates, and custom regexes —
  with NO claim to find names, addresses, or all PII. Reports show
  detector, column, count, validation method, and MASKED samples; full
  card/SSN values never appear anywhere. Redactions (each previewed with
  masked examples, each one undo step, each targeting one explicitly
  selected finding): fixed replacement, keep-last-N, full mask,
  HMAC-SHA-256 pseudonymization (per-run secret that is never stored,
  CSPRNG salts echoed back for reuse), remove column, remove rows — plus
  exporting only the non-PII columns through the ordinary export flow. A
  local-only audit log records counts and kinds, never values; nothing
  leaves the device.
- **Batch recipes** (palette → "Batch process files…"): apply a saved,
  VERSIONED, declarative step sequence — parse settings, profile
  validation, filter, transform, deduplicate, select columns, sort,
  export — to many files or a whole folder. No scripting, no shell, no
  network; inputs are only the explicitly selected files; outputs go only
  into the chosen folder with a `{name}`/`{ext}` template validated
  against path traversal; nothing is overwritten unless explicitly
  allowed; dry runs perform every step but write nothing. Stop-on-error
  or continue-with-report, a bounded parallel-file option, cancellation,
  and a structured report covering EVERY input file with its exact
  outcome. Recipes save and load as JSON with a clear version-migration
  error.
- **Pivot, unpivot, and transpose** (palette → "Pivot / unpivot /
  transpose…"): reshape between wide and long forms into a NEW document.
  Unpivot keeps chosen identifier columns and melts the rest into
  attribute/value rows (blank-omission and source-row provenance
  options); pivot turns a column's distinct values into headers with a
  chosen aggregation (none / count / sum / mean / median / min / max /
  first / last) — deterministic sorted column order, duplicate
  row-key/header coordinates detected, and "none" refuses multi-value
  cells with a clear error; transpose swaps rows and columns. Column
  counts are size-guarded with an explicit confirm, and pivot + unpivot
  round-trip when the data is losslessly representable.
- **Group-by aggregations** (palette → "Group by…"): summarise the active
  document into a NEW grouped document. Aggregates: row count, non-blank
  count, distinct count, sum, mean, min, max, median, first, last,
  concatenate, and concatenate-distinct — with custom output names,
  case-insensitive grouping (first-seen display value), keep-or-exclude
  blank-key handling, deterministic key / largest-first / first-seen
  ordering, configurable concatenation separator with a length cap, and
  all/visible-rows scope. Invalid numeric cells are ignored by the math
  but counted; group and distinct-tracking counts are bounded with clear
  errors. The preview shows the schema, group count, and sample rows.
- **Relational joins** (palette → "Join…"): join the active document with
  another open tab on ordered composite keys — inner, left/right/full
  outer, and left/right anti — into a NEW document; both sources stay
  untouched. Key matching reuses the comparison normalizations (trim,
  case, blank equivalence, numeric and date equivalence; blanks follow
  SQL NULL semantics unless told otherwise). Pick which right columns to
  include (collision-safe renaming with a configurable suffix), or use
  lookup mode, which requires unique right-side keys. The preview reports
  matched pairs, unmatched and duplicate-key counts per side, and the
  projected output size; one-to-many expansion past a threshold needs an
  explicit confirmation. Duplicate keys are never silently collapsed.
- **Multi-file append** (palette → "Append files…"): combine rows from open
  tabs, picked files, or a whole folder of delimited files into a NEW
  document — inputs are never modified. Columns align by exact name,
  case-insensitive name, position, or an explicit manual mapping; the
  output schema is the union, intersection, or the first input's columns.
  Optional source-file and source-row provenance columns (collision-safe
  names), duplicate-header rejection or tolerance, and stop-or-continue on
  failing inputs with a per-input outcome report. The preview shows the
  output schema, per-input mapped/missing columns, projected rows, and the
  projected backing; huge outputs automatically spill to disk and open as
  an indexed read-only document that cleans up after itself.
- **Outlier and anomaly finder** (palette → "Find outliers…"): flag
  suspicious values as statistical CANDIDATES, never verdicts. Numeric
  methods — IQR fences, MAD modified z-score (both robust, offered first),
  classic z-score, percentile bounds — plus categorical rare-value share,
  unexpected-values list, and regex pattern mismatch. Whole-column or
  group-wise (each group uses its own statistics); blanks and non-numeric
  cells are excluded from statistics, never flagged, and counted in the
  report; constant columns are safe (no division by zero). The report
  shows per-group summaries (count, median, bounds, flagged) and each
  flagged value with the reason. Actions: filter to candidates, export the
  JSON report, and previewed one-undo corrections — replace with blank,
  replace with group median, cap to bounds, remove rows. Scanning never
  marks the document dirty.
- **Missing-value repair** (palette → "Repair missing values…"): controlled
  fills and removals from a closed set — normalize null tokens (NA, N/A,
  null, …) to true blanks, constant fill, forward/backward fill with
  optional grouping columns (a fill never crosses a group boundary),
  mean/median/mode fill (ties → lexicographically smallest, invalid
  numerics ignored but counted), linear interpolation that never
  extrapolates unless enabled, and row/column removal above a
  missing-value threshold with explicit confirmation. Scopes to all,
  visible, or selected rows and chosen columns — hidden rows are never
  modified. Every operation previews affected counts, computed fill
  values, and before/after examples first, applies as ONE undo step, and
  undo restores the exact original representations, null tokens included.
- **Cross-column validation** (palette → "Validate across columns…"):
  relational rules BETWEEN columns from a closed, validated set — equals /
  differs, typed numeric comparison, typed date order, conditional required
  (with explicit blank-condition handling), exactly-one / at-least-one /
  mutually-exclusive populated, sum equality with absolute or percentage
  tolerance, and allowed value combinations. Rules reference columns by name
  and can be saved into a matching file profile. Scanning is a cancellable,
  read-only, revision-stamped job; violations list the involved values with
  a reason, support jump-to-row, filter-to-violations (per rule or overall),
  and JSON report export. Invalid rule configurations are rejected before
  any row is read, and numeric/date checks use typed coercion — never
  lexical comparison.
- **Semantic data-type detection** (palette → "Semantic types…"): recognise
  real-world value types beyond number/date/bool — email, URL, UUID, IPv4,
  IPv6, JSON, percentage, currency, phone number, postal code, and
  low-cardinality categorical columns. Each column reports the detected type
  with its confidence and matching/conflicting counts; a badge appears only
  at ≥95% matching over at least 10 non-blank cells, so low-confidence
  columns stay plain text. Detection never mutates data, and phone numbers
  and postal codes are never converted to numbers. Quick actions — filter to
  valid/invalid rows, normalize (lowercase emails/UUIDs), percentage →
  decimal, extract URL host or email domain into a new column — all show an
  exact preview first and apply as ONE undo step. Per-column overrides
  (including forcing plain text) persist into a matching file profile keyed
  by column NAME, so they survive rescans and reopening. Large indexed
  documents scan a labelled 100k-row sample. The cell editor gains a
  **Pretty-print JSON** button for cells that parse as JSON.
- **Command palette** (`Ctrl/Cmd+K`): fuzzy-search and run every CEESVEE
  action from the keyboard — file, editing, view, data, export, and tab
  commands, plus go to row/cell, opening recent files, and switching tabs.
  Commands that can't run right now stay listed with the reason (no document,
  read-only, nothing to undo, …).
- **Customizable keyboard shortcuts**: a shortcut editor (via the palette →
  "Keyboard shortcuts…") records new bindings per command, warns before
  reassigning a chord that another command already uses, and persists
  overrides in the settings file. Changes apply immediately.
- **Fuzzy value clustering** (palette → "Cluster values…"): find likely
  spelling, punctuation, spacing, and capitalization variants in a column
  using deterministic methods — key-collision fingerprint, n-gram
  fingerprint, Levenshtein distance, or Jaro-Winkler similarity — with
  case/whitespace/punctuation/accents/word-order normalization options.
  Review each cluster (members with frequencies, the shared key or score,
  rows affected), pick or type the canonical value, and apply all accepted
  clusters as ONE undo step. Nothing is ever merged automatically; stale
  results can't be applied after edits; the accepted mapping can be exported
  as JSON.
- **Compressed CSV support**: open `.csv.gz` / `.tsv.gz` files and `.zip`
  archives directly. ZIPs with several files show an entry chooser (sizes,
  compression ratio, sniffed delimiter and encoding); extraction streams with
  progress and cancellation, huge entries flow into indexed read-only mode,
  and suspicious compression ratios (decompression bombs) require explicit
  confirmation with a hard 8 GiB cap. Encrypted entries are rejected clearly.
  Archives are never edited in place — use Save As or Export — and exports
  or saves to a `*.gz` destination stream through gzip inside the same
  atomic-write pipeline.
- **Copy As** (`Ctrl/Cmd+Shift+C`): copy the selection or all visible rows as
  TSV, CSV (current or custom settings), JSON (objects, arrays, or JSON
  Lines), a Markdown table, or SQL `VALUES` rows — with or without headers.
  Serialization runs in Rust (off-screen and indexed rows read correctly),
  quotes/newlines/backslashes escape properly per format, blank cells become
  SQL `NULL`, and very large payloads ask before hitting the clipboard.
- **Paste Special** (`Ctrl/Cmd+Shift+V`): structured paste with an always-on
  preview (dimensions, added rows/columns, header changes, first ten rows,
  warnings). Modes: overwrite from the anchor or insert as new rows; options:
  transpose, skip blank source cells, trim incoming cells, repeat a smaller
  pattern over the selection, and treat the first pasted row as headers. The
  whole paste is one undo step and nothing mutates until Apply.
- **Multiline / raw cell editor** (`F2`, `Ctrl/Cmd+Enter`, or right-click →
  Edit cell): a resizable editor over the COMPLETE cell content with line,
  character, and UTF-8 byte counts, plus an **Escaped** view that makes
  newlines, tabs, non-breaking spaces, zero-width and control characters, and
  U+FFFD visible — copyable without altering the stored value. Applying is
  one undo step; NUL characters are blocked with a warning; indexed
  (read-only) documents allow inspection and copying only. The right-click
  menu also copies a cell's full value straight from the backend.

## [0.3.0]

### Added

- **Indexed read-only mode for huge files.** Files whose estimated in-memory
  size crosses the safety line open against a streaming record index instead of
  being loaded whole, so multi-gigabyte CSVs browse smoothly with bounded
  memory. An open-mode dialog shows the size/row/memory estimate and offers
  read-only, full in-memory, or cancel; a one-click **Convert to editable**
  materialises the document later. Browsing, find, filter, export, diagnostics,
  profiling, duplicates, and compare all work in read-only mode; editing tools
  are cleanly disabled behind a "Read-only (indexed)" chip.
- **Data-fidelity diagnostics.** A panel that scans the document as a
  cancellable background job and reports import damage (malformed bytes,
  ragged records with their source line numbers), replacement characters,
  mixed-type columns, blank-heavy columns, edge whitespace, duplicate or empty
  headers, and more — each with samples, a jump-to-cell action, and one-click
  "filter to affected rows" where applicable.
- **Reopen with settings.** Change delimiter, encoding, or the header toggle
  against a live preview of how the file would re-read — including exactly
  which cells change — and apply only with explicit confirmation. Dirty
  documents are saved (or explicitly discarded) first, never silently
  reparsed.
- **External-change detection.** CEESVEE fingerprints the file on disk and,
  when another program changes it, offers reload / ignore / save-as / open the
  disk copy side by side instead of clobbering anything.
- **Quit protection.** Closing the window with unsaved tabs prompts to save
  all (aborting if any save fails), discard all, or cancel.
- **Scoped and split export.** Export everything, the visible (filtered) rows,
  selected rows/columns, or a cell range; optionally split the output into
  multiple files by row count, approximate file size, or the values of a
  column (one file per group), with an optional JSON manifest recording row
  counts and SHA-256 hashes per output.
- **Column explorer.** A per-column profiling panel: type distribution,
  blanks, distinct counts (exact, or estimated once cardinality explodes), top
  values, numeric quartiles, date extremes, and text-length stats — over all
  rows or just the visible ones — with click-to-filter directly from the
  panel. Profiles are cached per column and survive edits to other columns.
- **Data cleaning transforms.** Previewable, one-undo-step cleanups: trim and
  collapse whitespace, case changes, find/replace within a column, number and
  date normalization, blank-fill, split a column by delimiter, and merge
  columns. Every transform shows affected counts, before/after examples, and
  parse failures before anything is applied.
- **Duplicate finder.** Group rows by a multi-column key with trim /
  case-insensitive / whitespace-collapse / blank-key options; review sample
  groups, filter the grid to duplicates, export them, or remove them in one
  undoable step keeping the first, last, or most complete row.
- **Compare two documents.** Positional or keyed comparison with column
  mapping for renamed/reordered columns and value equivalences (numeric,
  date, blank, case, trim). Results classify every record as added, removed,
  changed, unchanged, or conflict (duplicate keys are surfaced, never silently
  paired), with side-by-side cell diffs, jump-to-source-row, and exports of
  each class or a JSON change report.
- **Per-document UI state.** Find, filter, selection, column widths, frozen
  columns, scroll position, and panel state now follow each tab instead of
  leaking between documents.
- **File profiles.** Save delimiter/encoding/header choices, expected columns,
  and validation rules (required, unique, type, regex, numeric range) under a
  name matched to file patterns; matching files suggest — or with opt-in,
  auto-apply — the profile, and a validation report checks any document
  against it.

### Changed

- **Saves are atomic.** Save/Save As stream through a temporary file that is
  fsynced and renamed into place, so a crash or full disk can never leave a
  half-written file; optional single or rolling `.bak` backups.
- Saves, exports, and every heavy scan (diagnostics, profiling, duplicates,
  compare, indexing) now run as cancellable background jobs with progress in
  the status bar, keeping the grid responsive.
- Exports to legacy encodings (e.g. Windows-1252) are checked up front and
  blocked with the exact offending cells listed, instead of silently
  substituting unmappable characters.
- Long-running operations are guarded by document revisions: results computed
  against an older state of the document are rejected rather than applied
  stale.
- Building from source now requires Rust 1.89+ (std file locking).

## [0.2.2]

### Fixed

- Toolbar tools are no longer cut off on narrow windows. Below ~770px the
  row/column and data tools collapse into a **More tools** menu that stacks
  them with labels under "Rows & columns" and "Data" headers; wider windows
  keep the full inline toolbar.
- Toolbar dropdown menus (Recent files, and the new More tools) no longer
  render underneath the main content.

## [0.2.1]

### Fixed

- The auto-update prompt now actually appears. The `dialog:allow-ask` and
  `dialog:allow-message` capability permissions were missing, so the "Update
  available" confirmation (and the unsaved-tab close prompt) were silently
  blocked. Installs from 0.2.1 onward can self-update; earlier versions must be
  updated to 0.2.1 manually once.

### Added

- A **Check for updates** toolbar button that reports the outcome — up to date,
  an available update, or the error — instead of only checking silently at
  launch.

## [0.2.0]

### Added

- Per-column data-type detection (number / date / boolean / text) shown as
  header badges, with numeric columns right-aligned and a **Column Summaries**
  panel reporting count, blanks, unique values, and numeric min / max / mean.
- **Row filtering** with an advanced query builder: nest AND/OR groups of
  conditions (contains, equals, numeric comparisons, is-empty, regex, and more).
  The status bar shows "N of M rows" with one-click clear. Filtering is a
  non-destructive view — Save always writes every row, never just the visible
  ones.
- **Frozen (pinned) leading columns** via the column header menu.
- **Drag-and-drop** a file onto the window to open it.

## [0.1.0] — Initial release

### Added

- Open CSV / TSV / delimited files in a virtualized, canvas-rendered grid that
  fetches only the visible row windows from the Rust core.
- Automatic delimiter detection (comma, tab, semicolon, pipe) with manual and
  custom overrides.
- Automatic encoding detection (UTF-8, UTF-16 LE/BE, Windows-1252) with override
  and correct BOM handling.
- "First row is header" toggle with a frozen header row.
- Tabs for multiple open files and a recent-files list.
- Inline cell editing with Excel-style keyboard navigation.
- Insert / delete / reorder rows; insert / delete / rename / reorder columns.
- Multi-cell selection with Excel-compatible copy/paste and a fill handle.
- Undo / redo backed by a Rust command-pattern stack (single-step paste).
- Save / Save As with explicit export options: delimiter, encoding, quoting
  style, line endings (LF/CRLF), and BOM.
- Multi-column sort (ascending/descending per key).
- Find & replace — plain text or regex, scoped to a selection or the whole file.
- Live selection statistics (count, sum, average, min, max) in the status bar.
- Light and dark themes that follow the OS preference.
- File associations for `.csv` / `.tsv` / `.tab` / `.psv`, with single-instance
  handling so "Open with CEESVEE" opens the file in a new tab of the running app.
- In-app auto-updates (cryptographically signed) that check GitHub Releases on
  launch and prompt to download and install a newer version.

[Unreleased]: https://github.com/soldforaloss/ceesvee/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/soldforaloss/ceesvee/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/soldforaloss/ceesvee/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/soldforaloss/ceesvee/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/soldforaloss/ceesvee/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/soldforaloss/ceesvee/releases/tag/v0.1.0
