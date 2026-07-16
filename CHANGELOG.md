# Changelog

All notable changes to CEESVEE are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
