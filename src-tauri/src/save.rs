//! Crash-safe file writes (F03): stage the output in a temporary file inside
//! the destination directory, flush + fsync it, optionally keep a `.bak` copy
//! of the previous destination, then atomically swap the staging file into
//! place. Any failure or cancellation before the swap leaves the destination
//! byte-for-byte untouched and removes the staging file.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::dto::BackupPolicy;
use crate::error::{AppError, AppResult};

/// Write `dest` atomically: `write` streams into a staging file in the same
/// directory (same filesystem, so the final rename is atomic), which is
/// fsynced and then swapped in. Returns whatever `write` returns (bytes).
pub fn atomic_write(
    dest: &Path,
    backup: BackupPolicy,
    write: impl FnOnce(&mut File) -> AppResult<u64>,
) -> AppResult<u64> {
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| AppError::invalid("destination has no parent directory"))?;

    let mut staging = tempfile::Builder::new()
        .prefix(".ceesvee-save-")
        .suffix(".tmp")
        .tempfile_in(dir)?;

    // Stream the content. On any error (I/O, encoding, cancellation) the
    // `NamedTempFile` guard removes the staging file on drop and the
    // destination has not been touched.
    let bytes = write(staging.as_file_mut())?;

    staging.as_file_mut().flush()?;
    // Push the bytes to the platform's storage before the rename, so a crash
    // right after the swap cannot leave a truncated destination.
    staging.as_file().sync_all()?;

    if backup == BackupPolicy::Single && dest.exists() {
        // Copy (not rename) so the destination remains present at every
        // instant; only the atomic swap below ever replaces it.
        std::fs::copy(dest, bak_path(dest))?;
    }

    staging.persist(dest).map_err(|e| AppError::Io(e.error))?;
    Ok(bytes)
}

/// `data.csv` -> `data.csv.bak`, next to the destination.
pub fn bak_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    dest.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_stray_temp_files(dir: &Path) -> bool {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .all(|e| !e.file_name().to_string_lossy().contains(".ceesvee-save-"))
    }

    #[test]
    fn writes_new_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let bytes = atomic_write(&dest, BackupPolicy::None, |f| {
            f.write_all(b"a,b\n1,2\n")?;
            Ok(8)
        })
        .unwrap();
        assert_eq!(bytes, 8);
        assert_eq!(std::fs::read(&dest).unwrap(), b"a,b\n1,2\n");
        assert!(no_stray_temp_files(dir.path()));
    }

    #[test]
    fn replaces_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        std::fs::write(&dest, b"old").unwrap();
        atomic_write(&dest, BackupPolicy::None, |f| {
            f.write_all(b"new")?;
            Ok(3)
        })
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert!(!bak_path(&dest).exists(), "no backup was requested");
    }

    #[test]
    fn injected_failure_leaves_destination_untouched_and_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        std::fs::write(&dest, b"precious").unwrap();

        let result = atomic_write(&dest, BackupPolicy::None, |f| {
            // Write a partial chunk, then fail (as a disk-full or an
            // unmappable-character error would).
            f.write_all(b"partial garbage")?;
            Err(AppError::invalid("injected failure"))
        });
        assert!(result.is_err());
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"precious",
            "destination must be untouched after a failed write"
        );
        assert!(
            no_stray_temp_files(dir.path()),
            "failed staging files must be cleaned up"
        );
    }

    #[test]
    fn cancellation_removes_the_staging_file() {
        // Cancellation surfaces as Err(Cancelled) from the write closure —
        // identical cleanup path to any other failure.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let result = atomic_write(&dest, BackupPolicy::None, |f| {
            f.write_all(b"half a file")?;
            Err(AppError::Cancelled)
        });
        assert!(matches!(result, Err(AppError::Cancelled)));
        assert!(
            !dest.exists(),
            "cancelled first save must not create the file"
        );
        assert!(no_stray_temp_files(dir.path()));
    }

    #[test]
    fn single_backup_policy_keeps_prior_contents() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        std::fs::write(&dest, b"version-1").unwrap();

        atomic_write(&dest, BackupPolicy::Single, |f| {
            f.write_all(b"version-2")?;
            Ok(9)
        })
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"version-2");
        assert_eq!(std::fs::read(bak_path(&dest)).unwrap(), b"version-1");

        // A second save rolls the backup forward to version-2.
        atomic_write(&dest, BackupPolicy::Single, |f| {
            f.write_all(b"version-3")?;
            Ok(9)
        })
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"version-3");
        assert_eq!(std::fs::read(bak_path(&dest)).unwrap(), b"version-2");
    }

    #[test]
    fn backup_is_not_created_when_destination_is_new() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("fresh.csv");
        atomic_write(&dest, BackupPolicy::Single, |f| {
            f.write_all(b"x")?;
            Ok(1)
        })
        .unwrap();
        assert!(!bak_path(&dest).exists());
    }

    #[test]
    fn bak_path_appends_to_the_full_name() {
        assert_eq!(
            bak_path(Path::new("C:/data/report.csv"))
                .file_name()
                .unwrap(),
            "report.csv.bak"
        );
    }
}
