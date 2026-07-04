//! External-path declarations (issue #540): a cookbook may declare that an
//! environment depends on additional host filesystem paths beyond its own
//! project directory to function correctly -- e.g. a git worktree's `.git`
//! is a pointer file into a *separate* main repo, which holds the shared
//! object database and refs the worktree depends on.
//!
//! Cookbooks own tool-specific knowledge (git, in this example); isolation
//! backends own what to *do* with a required path (bind-mount it, or nothing
//! at all on the host path). Neither needs to know about the other: a
//! cookbook reports plain paths, with no notion of containers or mounting.
//!
//! Mirrors [`crate::gear`]'s drop-in-file pattern (one file per declaring
//! source, merged by the reader) but for a distinct concern: gear is what an
//! env *offers* consumers, external paths are what an env *requires* to
//! work at all.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Wire version of the external-paths schema. Bumped on **breaking** wire
/// changes only -- see [`crate::gear::SCHEMA_VERSION`] for the same
/// convention.
pub const SCHEMA_VERSION: u32 = 1;

/// Subdirectory inside an env where external-path files live. Each
/// declaring cookbook drops its contribution as a single file under this
/// directory; the reader merges them (plain set-union, no collision
/// concept -- unlike gear, there's no ambiguity in two sources requiring
/// the same or different paths).
pub const EXTERNAL_PATHS_DIR_NAME: &str = "external-paths.d";

/// Resolve the external-paths drop-in directory for an env.
pub fn external_paths_dir(env_dir: &Path) -> PathBuf {
    env_dir.join(EXTERNAL_PATHS_DIR_NAME)
}

/// Filename a given cookbook should write into `external-paths.d/`. Stable
/// so the reader and writer agree on per-cookbook ownership of the file.
pub fn external_paths_filename(cookbook_name: &str) -> String {
    format!("cookbook-{cookbook_name}.json")
}

fn deserialize_supported_version<'de, D: serde::Deserializer<'de>>(de: D) -> Result<u32, D::Error> {
    let v = u32::deserialize(de)?;
    if v == SCHEMA_VERSION {
        Ok(v)
    } else {
        Err(serde::de::Error::custom(format!(
            "unsupported external-paths schema version {v}; this build handles version {SCHEMA_VERSION}"
        )))
    }
}

/// Wire format: the JSON contents of one `external-paths.d/cookbook-X.json`
/// file. Cookbooks construct this and serialize it to stdout; readers
/// deserialize it via serde.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ExternalPathsFileData {
    #[serde(deserialize_with = "deserialize_supported_version")]
    pub version: u32,
    pub paths: Vec<String>,
}

/// Read every `*.json` file in `<env_dir>/external-paths.d/` in
/// lexicographic order, merge their `paths` lists, and dedup.
///
/// Returns an empty `Vec` if the directory doesn't exist. A file that fails
/// to read or parse is logged at `WARN` and skipped -- one bad file does not
/// prevent the rest from loading, and a missing/broken declaration should
/// never block a launch (best-effort, same contract as `gear`'s trait
/// default).
pub fn load_external_paths(env_dir: &Path) -> Vec<String> {
    let dir = external_paths_dir(env_dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::warn!(error = %err, dir = %dir.display(), "Could not read external-paths.d, continuing");
            return Vec::new();
        }
    };

    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();

    let mut paths = Vec::new();
    for file in files {
        let contents = match std::fs::read_to_string(&file) {
            Ok(contents) => contents,
            Err(err) => {
                tracing::warn!(error = %err, file = %file.display(), "Could not read external-paths file, skipping");
                continue;
            }
        };
        match serde_json::from_str::<ExternalPathsFileData>(&contents) {
            Ok(data) => paths.extend(data.paths),
            Err(err) => {
                tracing::warn!(error = %err, file = %file.display(), "Could not parse external-paths file, skipping");
            }
        }
    }

    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, paths: &[&str]) {
        let data = ExternalPathsFileData {
            version: SCHEMA_VERSION,
            paths: paths.iter().map(|p| p.to_string()).collect(),
        };
        std::fs::create_dir_all(external_paths_dir(dir)).unwrap();
        std::fs::write(
            external_paths_dir(dir).join(external_paths_filename(name)),
            serde_json::to_vec(&data).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn load_returns_empty_when_the_directory_is_absent() {
        let env_dir = tempfile::tempdir().unwrap();
        assert_eq!(load_external_paths(env_dir.path()), Vec::<String>::new());
    }

    #[test]
    fn load_merges_and_dedups_across_files() {
        let env_dir = tempfile::tempdir().unwrap();
        write(env_dir.path(), "git", &["/repo/a", "/repo/b"]);
        write(env_dir.path(), "github", &["/repo/b", "/repo/c"]);
        assert_eq!(
            load_external_paths(env_dir.path()),
            vec![
                "/repo/a".to_string(),
                "/repo/b".to_string(),
                "/repo/c".to_string()
            ]
        );
    }

    #[test]
    fn load_skips_an_unparsable_file_and_keeps_the_rest() {
        let env_dir = tempfile::tempdir().unwrap();
        write(env_dir.path(), "good", &["/repo/a"]);
        std::fs::write(
            external_paths_dir(env_dir.path()).join("cookbook-bad.json"),
            b"not json",
        )
        .unwrap();
        assert_eq!(
            load_external_paths(env_dir.path()),
            vec!["/repo/a".to_string()]
        );
    }

    #[test]
    fn load_rejects_an_unsupported_schema_version() {
        let env_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(external_paths_dir(env_dir.path())).unwrap();
        std::fs::write(
            external_paths_dir(env_dir.path()).join("cookbook-future.json"),
            br#"{"version":99,"paths":["/repo/a"]}"#,
        )
        .unwrap();
        // A file from a newer, incompatible schema is skipped, not trusted.
        assert_eq!(load_external_paths(env_dir.path()), Vec::<String>::new());
    }
}
