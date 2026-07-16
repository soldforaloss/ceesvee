//! Scoped and split exports (F04): resolve what slice of the document to
//! write, plan the output files (row chunks, byte budgets, or per-group
//! files with sanitized names), stream each through the atomic-write
//! pipeline, and optionally record a manifest with row counts and SHA-256
//! hashes per output.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::document::Document;
use crate::dto::{
    ExportManifest, ExportOptions, ExportOptionsEcho, ExportScope, ManifestOutput, ScopeCounts,
    SplitOptions,
};
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;
use crate::{export, save};

/// The rows (absolute indices) and columns an export will write, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedScope {
    pub rows: Vec<usize>,
    pub cols: Vec<usize>,
}

/// Resolve a display-space scope against the current document. Row indices in
/// the scope refer to what the user sees (the filtered view when one is
/// active); the result holds absolute indices.
pub fn resolve_scope(doc: &Document, scope: &ExportScope) -> AppResult<ResolvedScope> {
    let all_cols: Vec<usize> = (0..doc.n_cols()).collect();
    let abs = |display: usize| -> AppResult<usize> {
        doc.display_to_abs(display)
            .ok_or_else(|| AppError::invalid("selected row is out of range"))
    };

    match scope {
        ExportScope::All => Ok(ResolvedScope {
            rows: (0..doc.n_rows()).collect(),
            cols: all_cols,
        }),
        ExportScope::VisibleRows => Ok(ResolvedScope {
            rows: match doc.filter_view() {
                Some(view) => view.to_vec(),
                None => (0..doc.n_rows()).collect(),
            },
            cols: all_cols,
        }),
        ExportScope::SelectedRows { rows } => {
            if rows.is_empty() {
                return Err(AppError::invalid("no rows are selected"));
            }
            Ok(ResolvedScope {
                rows: rows.iter().map(|&d| abs(d)).collect::<AppResult<_>>()?,
                cols: all_cols,
            })
        }
        ExportScope::SelectedColumns { columns } => {
            if columns.is_empty() {
                return Err(AppError::invalid("no columns are selected"));
            }
            if columns.iter().any(|&c| c >= doc.n_cols()) {
                return Err(AppError::invalid("selected column is out of range"));
            }
            Ok(ResolvedScope {
                // All VISIBLE rows: exporting selected columns of a filtered
                // view exports what the user is looking at.
                rows: match doc.filter_view() {
                    Some(view) => view.to_vec(),
                    None => (0..doc.n_rows()).collect(),
                },
                // Preserve the user's selection order.
                cols: columns.clone(),
            })
        }
        ExportScope::SelectedRange { rect } => {
            if rect.width == 0 || rect.height == 0 {
                return Err(AppError::invalid("selection is empty"));
            }
            if rect.x.saturating_add(rect.width) > doc.n_cols() {
                return Err(AppError::invalid("selected range is out of range"));
            }
            let rows = (rect.y..rect.y + rect.height)
                .map(abs)
                .collect::<AppResult<_>>()?;
            Ok(ResolvedScope {
                rows,
                cols: (rect.x..rect.x + rect.width).collect(),
            })
        }
    }
}

/// Expected output shape, shown in the export dialog before writing.
pub fn scope_counts(doc: &Document, scope: &ExportScope) -> AppResult<ScopeCounts> {
    let resolved = resolve_scope(doc, scope)?;
    Ok(ScopeCounts {
        rows: resolved.rows.len(),
        cols: resolved.cols.len(),
    })
}

// ----- output planning --------------------------------------------------------

/// One planned output file: its path and the absolute rows it will contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedOutput {
    pub path: PathBuf,
    pub rows: Vec<usize>,
}

/// Split the resolved rows into output files. Every input row lands in
/// exactly one output, in source order.
pub fn plan_outputs(
    doc: &Document,
    base: &Path,
    resolved: &ResolvedScope,
    split: &SplitOptions,
) -> AppResult<Vec<PlannedOutput>> {
    match split {
        SplitOptions::None => Ok(vec![PlannedOutput {
            path: base.to_path_buf(),
            rows: resolved.rows.clone(),
        }]),
        SplitOptions::MaxRows { rows_per_file } => {
            if *rows_per_file == 0 {
                return Err(AppError::invalid("rows per file must be at least 1"));
            }
            let chunks: Vec<Vec<usize>> = resolved
                .rows
                .chunks(*rows_per_file)
                .map(<[usize]>::to_vec)
                .collect();
            Ok(numbered_outputs(base, chunks))
        }
        SplitOptions::ApproximateBytes { max_bytes } => {
            if *max_bytes == 0 {
                return Err(AppError::invalid("byte budget must be positive"));
            }
            let rows = doc.rows();
            let mut chunks: Vec<Vec<usize>> = Vec::new();
            let mut current: Vec<usize> = Vec::new();
            let mut current_bytes: u64 = 0;
            for &r in &resolved.rows {
                let row_bytes: u64 = resolved
                    .cols
                    .iter()
                    .map(|&c| rows[r][c].len() as u64 + 1)
                    .sum::<u64>()
                    + 1;
                // Never split a row; a single row larger than the budget gets
                // its own file.
                if !current.is_empty() && current_bytes + row_bytes > *max_bytes {
                    chunks.push(std::mem::take(&mut current));
                    current_bytes = 0;
                }
                current.push(r);
                current_bytes += row_bytes;
            }
            if !current.is_empty() {
                chunks.push(current);
            }
            Ok(numbered_outputs(base, chunks))
        }
        SplitOptions::GroupByColumn { column } => {
            if *column >= doc.n_cols() {
                return Err(AppError::invalid("group column is out of range"));
            }
            let rows = doc.rows();
            // Group rows by value, preserving first-seen group order and
            // source row order within each group.
            let mut order: Vec<String> = Vec::new();
            let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
            for &r in &resolved.rows {
                let value = &rows[r][*column];
                if !groups.contains_key(value) {
                    order.push(value.clone());
                }
                groups.entry(value.clone()).or_default().push(r);
            }

            // Sanitize names; resolve collisions deterministically by
            // first-seen order (-2, -3, …).
            let mut used: HashMap<String, usize> = HashMap::new();
            let mut outputs = Vec::with_capacity(order.len());
            for value in order {
                let group_rows = groups.remove(&value).unwrap_or_default();
                let mut label = sanitize_filename_part(&value);
                let n = used.entry(label.clone()).or_insert(0);
                *n += 1;
                if *n > 1 {
                    label = format!("{label}-{n}");
                }
                outputs.push(PlannedOutput {
                    path: derived_path(base, &label),
                    rows: group_rows,
                });
            }
            Ok(outputs)
        }
    }
}

/// `data.csv` + chunks -> `data-001.csv`, `data-002.csv`, … (zero-padded to
/// the chunk count's width). A single chunk keeps the base name.
fn numbered_outputs(base: &Path, chunks: Vec<Vec<usize>>) -> Vec<PlannedOutput> {
    if chunks.len() <= 1 {
        return chunks
            .into_iter()
            .map(|rows| PlannedOutput {
                path: base.to_path_buf(),
                rows,
            })
            .collect();
    }
    let width = chunks.len().to_string().len().max(3);
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, rows)| PlannedOutput {
            path: derived_path(base, &format!("{:0width$}", i + 1)),
            rows,
        })
        .collect()
}

/// `data.csv` + "east" -> `data-east.csv` (same directory, same extension).
fn derived_path(base: &Path, label: &str) -> PathBuf {
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "export".to_string());
    let ext = base
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    base.with_file_name(format!("{stem}-{label}{ext}"))
}

/// Windows reserved device names (case-insensitive, extension ignored).
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Turn an arbitrary group value into a filename fragment that is valid on
/// Windows, macOS and Linux: control characters and `<>:"/\|?*` become `_`,
/// trailing dots/spaces are trimmed, reserved device names are prefixed, long
/// values are truncated, and blanks become "(blank)".
pub fn sanitize_filename_part(value: &str) -> String {
    const MAX_LEN: usize = 64;
    let mut out: String = value
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .take(MAX_LEN)
        .collect();
    // Windows rejects names ending in dots or spaces.
    while out.ends_with(['.', ' ']) {
        out.pop();
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return "(blank)".to_string();
    }
    if RESERVED_NAMES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(trimmed))
    {
        return format!("_{trimmed}");
    }
    trimmed.to_string()
}

// ----- writing -----------------------------------------------------------------

/// A writer that hashes everything passing through it (for the manifest).
struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Stream every planned output through the atomic-write pipeline, then write
/// the manifest (also atomically) when requested. Returns the manifest data.
pub fn run_export(
    doc: &Document,
    base: &Path,
    options: &ExportOptions,
    scope: &ExportScope,
    split: &SplitOptions,
    write_manifest: bool,
    ctx: &JobCtx,
) -> AppResult<ExportManifest> {
    let resolved = resolve_scope(doc, scope)?;
    let outputs = plan_outputs(doc, base, &resolved, split)?;
    ctx.set_total(resolved.rows.len() as u64);

    let mut recorded = Vec::with_capacity(outputs.len());
    for (i, output) in outputs.iter().enumerate() {
        ctx.set_part((i + 1) as u32);
        let mut hash_hex = String::new();
        save::atomic_write(&output.path, options.backup, |file| {
            let mut hashing = HashingWriter {
                inner: file,
                hasher: Sha256::new(),
            };
            let bytes = export::write_view(
                doc,
                &output.rows,
                &resolved.cols,
                options,
                &mut hashing,
                Some(ctx),
            )?;
            hash_hex = format!("{:x}", hashing.hasher.finalize());
            Ok(bytes)
        })?;
        recorded.push(ManifestOutput {
            file_name: output
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            rows: output.rows.len(),
            sha256: hash_hex,
        });
    }

    let manifest = ExportManifest {
        source_file_name: doc
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string()),
        source_fingerprint: doc.fingerprint(),
        scope: scope.clone(),
        split: split.clone(),
        options: ExportOptionsEcho::from(options),
        outputs: recorded,
    };

    if write_manifest {
        let json = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| AppError::Other(format!("manifest serialization failed: {e}")))?;
        let manifest_path = manifest_path(base);
        save::atomic_write(&manifest_path, options.backup, |file| {
            file.write_all(&json)?;
            Ok(json.len() as u64)
        })?;
    }

    Ok(manifest)
}

/// `data.csv` -> `data.csv.manifest.json`, next to the (first) output.
pub fn manifest_path(base: &Path) -> PathBuf {
    let mut name = base.file_name().unwrap_or_default().to_os_string();
    name.push(".manifest.json");
    base.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::CellRect;
    use crate::job::JobRegistry;
    use crate::parse::{parse, ParseSettings};

    fn doc_from(csv: &str, has_header: bool) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, has_header)
    }

    fn options() -> ExportOptions {
        ExportOptions {
            delimiter: ",".into(),
            encoding: "UTF-8".into(),
            quote_style: "minimal".into(),
            line_ending: "lf".into(),
            bom: false,
            include_headers: true,
            backup: Default::default(),
        }
    }

    fn ctx() -> (JobRegistry, JobCtx) {
        let registry = JobRegistry::default();
        let ctx = registry.begin("export", Some(1), |_| {});
        (registry, ctx)
    }

    #[test]
    fn visible_rows_scope_respects_the_filter() {
        let mut d = doc_from("n\n1\n2\n3\n4", true);
        d.set_filter(vec![1, 3]);
        let resolved = resolve_scope(&d, &ExportScope::VisibleRows).unwrap();
        assert_eq!(resolved.rows, vec![1, 3]);
        assert_eq!(resolved.cols, vec![0]);
    }

    #[test]
    fn selected_rows_are_display_indices() {
        let mut d = doc_from("n\n10\n20\n30\n40", true);
        d.set_filter(vec![2, 3]); // visible: rows "30", "40"
        let resolved = resolve_scope(&d, &ExportScope::SelectedRows { rows: vec![0, 1] }).unwrap();
        assert_eq!(resolved.rows, vec![2, 3], "display -> absolute mapping");
        assert!(resolve_scope(&d, &ExportScope::SelectedRows { rows: vec![9] }).is_err());
    }

    #[test]
    fn selected_columns_preserve_user_order() {
        let d = doc_from("a,b,c\n1,2,3", true);
        let resolved = resolve_scope(
            &d,
            &ExportScope::SelectedColumns {
                columns: vec![2, 0],
            },
        )
        .unwrap();
        assert_eq!(resolved.cols, vec![2, 0]);
    }

    #[test]
    fn selected_range_resolves_rows_and_cols() {
        let d = doc_from("a,b,c\n1,2,3\n4,5,6\n7,8,9", true);
        let resolved = resolve_scope(
            &d,
            &ExportScope::SelectedRange {
                rect: CellRect {
                    x: 1,
                    y: 1,
                    width: 2,
                    height: 2,
                },
            },
        )
        .unwrap();
        assert_eq!(resolved.rows, vec![1, 2]);
        assert_eq!(resolved.cols, vec![1, 2]);
    }

    #[test]
    fn max_rows_split_never_duplicates_or_omits() {
        let d = doc_from("n\n0\n1\n2\n3\n4\n5\n6", true);
        let resolved = resolve_scope(&d, &ExportScope::All).unwrap();
        let outputs = plan_outputs(
            &d,
            Path::new("out.csv"),
            &resolved,
            &SplitOptions::MaxRows { rows_per_file: 3 },
        )
        .unwrap();
        assert_eq!(outputs.len(), 3);
        let all: Vec<usize> = outputs.iter().flat_map(|o| o.rows.clone()).collect();
        assert_eq!(all, resolved.rows, "concatenation reproduces the source");
        assert_eq!(outputs[0].path, Path::new("out-001.csv"));
        assert_eq!(outputs[2].path, Path::new("out-003.csv"));
        assert_eq!(outputs[2].rows.len(), 1, "last chunk holds the remainder");
    }

    #[test]
    fn approximate_bytes_split_keeps_rows_whole() {
        let d = doc_from("v\naaaaaaaaaa\nbb\ncccccccccc\ndd", true);
        let resolved = resolve_scope(&d, &ExportScope::All).unwrap();
        let outputs = plan_outputs(
            &d,
            Path::new("out.csv"),
            &resolved,
            &SplitOptions::ApproximateBytes { max_bytes: 14 },
        )
        .unwrap();
        let all: Vec<usize> = outputs.iter().flat_map(|o| o.rows.clone()).collect();
        assert_eq!(all, resolved.rows);
        assert!(outputs.len() >= 2);
        // A row bigger than the budget still lands somewhere, alone.
        let tiny = plan_outputs(
            &d,
            Path::new("out.csv"),
            &resolved,
            &SplitOptions::ApproximateBytes { max_bytes: 1 },
        )
        .unwrap();
        assert_eq!(tiny.len(), 4, "each row gets its own file");
    }

    #[test]
    fn group_split_orders_and_sanitizes() {
        let d = doc_from(
            "region,v\neast,1\nwest,2\neast,3\n,4\na/b,5\na_b,6\nCON,7",
            true,
        );
        let resolved = resolve_scope(&d, &ExportScope::All).unwrap();
        let outputs = plan_outputs(
            &d,
            Path::new("out.csv"),
            &resolved,
            &SplitOptions::GroupByColumn { column: 0 },
        )
        .unwrap();
        let names: Vec<String> = outputs
            .iter()
            .map(|o| o.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "out-east.csv",
                "out-west.csv",
                "out-(blank).csv",
                "out-a_b.csv",
                "out-a_b-2.csv",
                "out-_CON.csv",
            ]
        );
        // east preserves source order (rows 0 and 2).
        assert_eq!(outputs[0].rows, vec![0, 2]);
        // Every row present exactly once.
        let mut all: Vec<usize> = outputs.iter().flat_map(|o| o.rows.clone()).collect();
        all.sort_unstable();
        assert_eq!(all, (0..7).collect::<Vec<_>>());
    }

    #[test]
    fn sanitizer_produces_cross_platform_names() {
        assert_eq!(sanitize_filename_part("north/south"), "north_south");
        assert_eq!(
            sanitize_filename_part("a<b>c:d\"e|f?g*h\\i"),
            "a_b_c_d_e_f_g_h_i"
        );
        assert_eq!(sanitize_filename_part("  "), "(blank)");
        assert_eq!(sanitize_filename_part("ends. . ."), "ends");
        assert_eq!(sanitize_filename_part("nul"), "_nul");
        assert_eq!(sanitize_filename_part("Ok Name"), "Ok Name");
        let long = "x".repeat(200);
        assert!(sanitize_filename_part(&long).len() <= 64);
    }

    #[test]
    fn run_export_writes_exact_rows_and_matching_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("subset.csv");
        let mut d = doc_from("n,v\n0,a\n1,b\n2,c\n3,d\n4,e", true);
        d.set_filter(vec![0, 2, 4]); // 3 visible rows

        let (_r, ctx) = ctx();
        let manifest = run_export(
            &d,
            &base,
            &options(),
            &ExportScope::VisibleRows,
            &SplitOptions::None,
            true,
            &ctx,
        )
        .unwrap();

        let text = std::fs::read_to_string(&base).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "header + exactly 3 visible data rows");
        assert_eq!(lines[1], "0,a");
        assert_eq!(lines[2], "2,c");
        assert_eq!(lines[3], "4,e");

        assert_eq!(manifest.outputs.len(), 1);
        assert_eq!(manifest.outputs[0].rows, 3);

        // Manifest hash matches the file on disk.
        use sha2::{Digest, Sha256};
        let bytes = std::fs::read(&base).unwrap();
        let expected = format!("{:x}", Sha256::digest(&bytes));
        assert_eq!(manifest.outputs[0].sha256, expected);

        // The manifest file itself exists and round-trips as JSON.
        let mpath = manifest_path(&base);
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&mpath).unwrap()).unwrap();
        assert_eq!(json["outputs"][0]["rows"], 3);
        assert_eq!(json["scope"]["type"], "visibleRows");
    }

    #[test]
    fn split_export_writes_each_part_and_hashes_match() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("parts.csv");
        let d = doc_from("n\n0\n1\n2\n3\n4", true);

        let (_r, ctx) = ctx();
        let manifest = run_export(
            &d,
            &base,
            &options(),
            &ExportScope::All,
            &SplitOptions::MaxRows { rows_per_file: 2 },
            true,
            &ctx,
        )
        .unwrap();
        assert_eq!(manifest.outputs.len(), 3);

        use sha2::{Digest, Sha256};
        let mut total_rows = 0usize;
        for output in &manifest.outputs {
            let path = dir.path().join(&output.file_name);
            let bytes = std::fs::read(&path).unwrap();
            let hash = format!("{:x}", Sha256::digest(&bytes));
            assert_eq!(&hash, &output.sha256, "{}", output.file_name);
            // header + rows lines
            let lines = String::from_utf8(bytes).unwrap().lines().count();
            assert_eq!(lines, output.rows + 1);
            total_rows += output.rows;
        }
        assert_eq!(total_rows, 5, "no duplicated or omitted rows");
    }

    #[test]
    fn cancelled_export_cleans_up_current_part() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("cancel.csv");
        let d = doc_from("n\n1\n2\n3", true);
        let registry = JobRegistry::default();
        let ctx = registry.begin("export", Some(1), |_| {});
        registry.cancel(ctx.id);
        let result = run_export(
            &d,
            &base,
            &options(),
            &ExportScope::All,
            &SplitOptions::None,
            false,
            &ctx,
        );
        assert!(matches!(result, Err(AppError::Cancelled)));
        assert!(!base.exists());
        assert!(
            std::fs::read_dir(dir.path()).unwrap().count() == 0,
            "no temp litter"
        );
    }
}
