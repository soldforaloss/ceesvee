<div align="center">

<img src="branding/ceesvee-icon.svg" alt="CEESVEE" width="104" height="104" />

# CEESVEE

**A fast, open-source CSV / delimited-file viewer and editor.**

Open a million-row file in a blink, edit it like a spreadsheet, and save it back
without surprises. Built with [Tauri](https://tauri.app), Rust, and React.

[![CI](https://github.com/soldforaloss/ceesvee/actions/workflows/ci.yml/badge.svg)](https://github.com/soldforaloss/ceesvee/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-violet.svg)](LICENSE)
[![Built with Tauri](https://img.shields.io/badge/built%20with-Tauri%20v2-24C8DB)](https://tauri.app)

![CEESVEE screenshot](docs/screenshot.png)

</div>

---

## Why CEESVEE?

Most spreadsheet apps choke on large CSVs, mangle your delimiters, or silently
"helpfully" reformat your data. CEESVEE is built around one priority: **be fast
and faithful on large, real-world delimited files.**

- ⚡ **Fast on huge files.** The dataset lives in Rust; the UI is canvas-rendered
  and only ever fetches the rows it's about to draw. Opening and smoothly
  scrolling a **1,000,000-row / 100 MB+** file is a core requirement, not a
  stretch goal.
- 🧭 **Faithful round-trips.** Parse → edit → save preserves your data. You
  control delimiter, encoding, quoting, line endings, and BOM on export.
- ⌨️ **Keyboard-first.** Excel-style navigation and shortcuts so it feels
  instantly familiar.

## Features

**Viewing**

- Open CSV / TSV / and other delimited files in a virtualized, spreadsheet-style grid.
- Open **compressed files** — `.csv.gz` and `.zip` archives (with an entry
  chooser) — and export back to gzip. Decompression-bomb guards included.
- Auto-detect the **delimiter** (comma, tab, semicolon, pipe) with a manual /
  custom override — plus an **advanced import** for preambles, comment
  lines, custom quoting/escaping, multi-row headers, and footers.
- Auto-detect the **encoding** (UTF-8, UTF-16 LE/BE, Windows-1252 / Latin-1) with
  an override, plus correct **BOM** handling.
- "First row is header" toggle with a frozen header row.
- Tabs for multiple open files, plus a recent-files list.
- **Follow mode** — tail a growing CSV log live (read-only), with
  pause/resume, new-row filtering, and rotation/truncation detection.
- Status bar with row/column counts, encoding, delimiter, line endings, and live
  selection stats (count, sum, average, min, max).

**Editing**

- Inline cell editing with Excel-like keyboard navigation.
- **Multiline / raw cell editor** (`F2`) with an Escaped view that reveals
  newlines, tabs, and invisible characters — safe to inspect and copy.
- Insert / delete / reorder rows; insert / delete / rename / reorder columns.
- Multi-cell selection and **Excel-compatible copy/paste** (TSV on the clipboard).
- **Copy As** JSON / Markdown / SQL / CSV variants, and **Paste Special** with
  transpose, skip-blanks, pattern repeat, and insert-as-rows — all previewed.
- **Undo / redo** backed by a Rust undo stack (Ctrl+Z / Ctrl+Y).
- **Save / Save As** with explicit export options: delimiter, encoding, quoting
  style, line endings (LF/CRLF), and BOM.

**Navigate & analyze**

- **Command palette** (`Ctrl/Cmd+K`) — fuzzy-search and run every action from
  the keyboard: commands, go to row/cell, recent files, tab switching. Every
  shortcut is customizable via the built-in shortcut editor.
- **Fuzzy value clustering** — group spelling/punctuation/case variants
  (fingerprint, n-gram, Levenshtein, Jaro-Winkler) and normalize them in one
  reviewed, undoable step.
- **Semantic type detection** — recognize email / URL / UUID / IP / JSON /
  percentage / currency / phone / postal-code columns with confidence
  counts, filter to valid or invalid rows, and run previewed, undoable
  quick actions (normalize, percent→decimal, extract URL host / email
  domain). Overrides persist in file profiles.
- **Cross-column validation** — relational rules between columns (typed
  comparisons, date order, conditional required, sum equality with
  tolerance, allowed combinations, …) with violation samples, jump-to-row,
  filter-to-violations, and JSON reports. Rules persist in file profiles.
- **Missing-value repair** — normalize null tokens, constant /
  forward / backward / mean / median / mode fills, grouped fills that
  never cross boundaries, linear interpolation, and thresholded row or
  column removal — all previewed, scoped, and one undo step.
- **Outlier finder** — IQR / MAD / z-score / percentile and categorical
  rare-value or pattern checks, whole-column or group-wise, with
  reasons, group statistics, filtering, and previewed one-undo
  corrections (blank, median, cap, remove).
- **Append files** — concatenate open tabs, files, or a folder into a
  new document, aligning columns by name / case-insensitive name /
  position / manual mapping, with union or intersection schemas,
  provenance columns, and per-input reports. Inputs stay untouched;
  huge outputs open indexed automatically.
- **Joins & lookups** — inner / left / right / full / anti joins between
  two open documents on composite keys with trim / case / blank /
  numeric / date normalization, cardinality previews, expansion
  confirmation, and a unique-key lookup mode. Sources are preserved.
- **Group by** — count / distinct / sum / mean / min / max / median /
  first / last / concatenate aggregations into a new grouped document,
  with normalized grouping, blank-key policies, and ordering options.
- **Pivot / unpivot / transpose** — reshape wide↔long with aggregation
  choices, duplicate-coordinate detection, provenance, and size guards.
- **Batch recipes** — run a saved, declarative pipeline (validate /
  filter / clean / dedup / sort / export) over folders of files with
  dry runs, per-file reports, and no-overwrite defaults. No scripting.
- **PII detection & redaction** — find emails, phones, IPs, SSN
  patterns, and Luhn-valid card numbers (masked samples only), then
  mask, pseudonymize (HMAC, never-stored secret), or remove them —
  previewed, undoable, audited locally, never leaving your device.
- **Change inspector** — see every unsaved operation with before/after
  values, jump to changes, and selectively revert cells, columns,
  operations, or everything — each revert itself undoable.
- **Crash recovery** (opt-in) — an append-only local edit journal that
  replays your unsaved work after a crash, without ever touching the
  source file. Changed sources recover as copies; journals clean up on
  save and close.
- Multi-column **sort** (ascending/descending per key).
- **Find & replace** — plain text or regex, scoped to a selection or the whole file.

**Comfort**

- Light and dark themes that follow your OS preference.
- A restrained, dense-but-readable interface.
- **File associations** — set CEESVEE as the default app for `.csv` / `.tsv` /
  `.tab` / `.psv` files, or right-click → **Open with CEESVEE**. Opening another
  file while CEESVEE is running adds a tab instead of a second window.

## Install

> Pre-built installers are attached to each [GitHub Release](https://github.com/soldforaloss/ceesvee/releases).

| Platform    | Download                                 |
| ----------- | ---------------------------------------- |
| **Windows** | `.msi` or `.exe` (NSIS) installer        |
| **macOS**   | `.dmg` (Apple Silicon + Intel universal) |
| **Linux**   | `.AppImage` or `.deb`                    |

> **macOS note:** builds are currently **unsigned and un-notarized**. macOS will
> warn on first launch — right-click the app and choose **Open**, or run
> `xattr -dr com.apple.quarantine /Applications/CEESVEE.app`. Notarization
> requires a paid Apple Developer account and can be added later.

## Build from source

**Prerequisites**

- [Node.js](https://nodejs.org/) 18+ and npm
- [Rust](https://www.rust-lang.org/tools/install) (stable)
- Platform Tauri prerequisites — see the
  [Tauri v2 prerequisites guide](https://v2.tauri.app/start/prerequisites/).
  On Windows you need **WebView2** (preinstalled on Windows 11) and the
  **MSVC C++ build tools**; on Linux, the WebKitGTK 4.1 dev packages.

**Run in development**

```bash
git clone https://github.com/soldforaloss/ceesvee.git
cd ceesvee
npm install
npm run tauri dev
```

**Build installers**

```bash
npm run tauri build
```

The bundled installers are written to `src-tauri/target/release/bundle/`.

## Tech stack

| Layer   | Choice                                                                                                                                                                                                   |
| ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Shell   | **Tauri v2** — Rust core + system WebView, small binaries, cross-platform                                                                                                                                |
| Core    | **Rust** — [`csv`](https://crates.io/crates/csv) parsing/serialization, [`encoding_rs`](https://crates.io/crates/encoding_rs) + [`chardetng`](https://crates.io/crates/chardetng) for encoding detection |
| UI      | **React 18 + TypeScript** (strict), bundled with **Vite**                                                                                                                                                |
| Grid    | **[Glide Data Grid](https://github.com/glideapps/glide-data-grid)** — canvas-rendered, virtualized                                                                                                       |
| Styling | **Tailwind CSS v4**                                                                                                                                                                                      |
| State   | **Zustand**                                                                                                                                                                                              |

## Architecture

CEESVEE follows one rule: **Rust owns the data; the front end owns rendering.**
The front end never holds the whole file — it requests only the row windows it
needs to display.

```
┌───────────────────────────┐       invoke / IPC        ┌───────────────────────────┐
│  React + Glide Data Grid  │ ───────────────────────▶  │  Rust core (Tauri)        │
│  • virtualized grid        │  get_rows(start, count)   │  • parse + encoding         │
│  • only visible rows        │ ◀───────────────────────  │  • in-memory mutable model  │
│  • optimistic edits         │  rows window + dirty map  │  • dirty tracking           │
│  • copy / paste / find UI   │                           │  • undo / redo stack        │
└───────────────────────────┘                           └───────────────────────────┘
```

The Rust core exposes a small command surface (`open_file`, `get_rows`,
`set_cell`, `insert_rows`/`delete_rows`, `sort`, `find`/`replace_all`, `save`,
`undo`/`redo`, …). Heavy work — reading and parsing files — runs off the UI
thread so the interface never blocks.

See [`src-tauri/src`](src-tauri/src) for the core and [`src`](src) for the UI.

## Development

```bash
npm run tauri dev      # run the app with hot reload
npm run lint           # ESLint
npm run typecheck      # tsc --noEmit
npm test               # frontend unit tests (Vitest)

cargo test  --manifest-path src-tauri/Cargo.toml          # Rust unit tests
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo fmt   --manifest-path src-tauri/Cargo.toml --check
```

## Roadmap

- [ ] Column type detection and per-column summaries
- [ ] Filtering / query view
- [ ] Frozen columns (pin leading columns)
- [ ] Drag-and-drop to open files
- [ ] Signed & notarized macOS / Windows builds
- [ ] Large-file streaming export

Non-goals for v1: formulas, charts, scripting/macros, and cloud sync.

## Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) for
the workflow, coding standards (Conventional Commits, `clippy`/`fmt`,
ESLint/Prettier), and how to run the test suites.

## License

[MIT](LICENSE) © CEESVEE contributors.
