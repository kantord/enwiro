use std::fs;
use std::io;
use std::path::Path;

/// Write `data` to `path` atomically via a `.tmp` staging file.
/// Parent directories are created automatically.
///
/// The staging file is named by replacing `path`'s last extension with `.tmp`
/// (e.g. `meta.json` → `meta.tmp`, `recipes.cache` → `recipes.tmp`).
/// Callers that need a different tmp path must not use this function.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_atomic_write_creates_file_with_correct_contents() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("output.json");
        let data = b"{\"hello\": \"world\"}";

        atomic_write(&target, data).expect("atomic_write should succeed");

        let written = fs::read(&target).expect("target file should exist after atomic_write");
        assert_eq!(written, data);
    }

    #[test]
    fn test_atomic_write_leaves_no_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("output.json");
        let data = b"some content";

        atomic_write(&target, data).expect("atomic_write should succeed");

        let tmp = target.with_extension("tmp");
        assert!(
            !tmp.exists(),
            "the .tmp staging file should be gone after atomic_write succeeds"
        );
    }

    #[test]
    fn test_atomic_write_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("output.json");

        atomic_write(&target, b"first").unwrap();
        atomic_write(&target, b"second").unwrap();

        let written = fs::read(&target).unwrap();
        assert_eq!(written, b"second");
    }

    #[test]
    fn test_atomic_write_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a").join("b").join("c").join("output.json");

        atomic_write(&target, b"data").expect("atomic_write should create missing parent dirs");

        assert!(target.exists());
    }

    #[test]
    fn test_atomic_write_preserves_exact_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("output.bin");
        let data: Vec<u8> = (0u8..=255).collect();

        atomic_write(&target, &data).unwrap();

        let written = fs::read(&target).unwrap();
        assert_eq!(written, data);
    }
}
