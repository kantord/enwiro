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
}
