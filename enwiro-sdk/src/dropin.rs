//! Shared machinery for "drop-in directory" metadata: a directory of small
//! per-source JSON files that together describe one env. Both [`crate::gear`]
//! and [`crate::external_paths`] use this shape -- one file per contributing
//! cookbook, merged by a reader -- for two data kinds with different merge
//! semantics (gear treats a name collision as a hard error; external paths
//! has no such concept and just accumulates), so only the directory-listing
//! step is shared here; each caller owns its own merge.

use std::path::{Path, PathBuf};

/// List the `*.json` files directly inside `dir`, in lexicographic order
/// (callers read files in this order so merge conflicts are deterministic).
/// Returns an empty `Vec` if `dir` doesn't exist. Other read errors (e.g.
/// permission denied) propagate for the caller to handle -- best-effort
/// readers treat that as "no declarations found", stricter ones (like gear)
/// surface it.
pub fn list_json_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    Ok(paths)
}

/// Serialize `data` to JSON and write it to `path` (via [`crate::fs::atomic_write`]).
/// Returns whether it succeeded; fire-and-forget callers (writing a gear or
/// external-paths declaration) can ignore the result, callers that need to
/// gate further action on success (e.g. only firing a hook once its data
/// actually landed) check it. Logs at `DEBUG` on either failure -- a broken
/// or unwritable declaration should never propagate as a hard error.
pub fn write_json_file(path: &Path, data: &impl serde::Serialize) -> bool {
    let bytes = match serde_json::to_vec(data) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::debug!(error = %err, path = %path.display(), "Failed to serialize drop-in JSON, continuing");
            return false;
        }
    };
    if let Err(err) = crate::fs::atomic_write(path, &bytes) {
        tracing::debug!(error = %err, path = %path.display(), "Failed to write drop-in file, continuing");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_json_files_returns_empty_when_the_directory_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            list_json_files(&dir.path().join("does-not-exist")).unwrap(),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn list_json_files_is_sorted_and_ignores_non_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.json"), "").unwrap();
        std::fs::write(dir.path().join("a.json"), "").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        assert_eq!(
            list_json_files(dir.path()).unwrap(),
            vec![dir.path().join("a.json"), dir.path().join("b.json")]
        );
    }

    #[test]
    fn write_json_file_writes_readable_json_and_reports_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("declaration.json");
        assert!(write_json_file(&path, &vec!["a", "b"]));
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&contents).unwrap(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn write_json_file_reports_failure_when_the_parent_path_is_a_file() {
        // `atomic_write` creates missing parent directories, so force a
        // real failure a different way: a path component that's already a
        // plain file can never become a directory.
        let dir = tempfile::tempdir().unwrap();
        let blocking_file = dir.path().join("not-a-directory");
        std::fs::write(&blocking_file, b"").unwrap();
        let path = blocking_file.join("declaration.json");
        assert!(!write_json_file(&path, &vec!["a"]));
    }
}
