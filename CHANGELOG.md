# Changelog

All notable changes to CEESVEE are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/soldforaloss/ceesvee/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/soldforaloss/ceesvee/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/soldforaloss/ceesvee/releases/tag/v0.1.0
