use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Wire version of the gear schema. Bumped when `GearFile` / `Gear` /
/// `WebEntry` change shape. Cookbook authors should set
/// `GearFile { version: SCHEMA_VERSION, ... }` to ride future upgrades
/// rather than hardcoding a literal.
pub const SCHEMA_VERSION: u32 = 1;

/// Subdirectory inside an env where gear files live. Each cookbook drops
/// its contribution as a single file under this directory; the reader
/// merges them gear-atomically (see `read_gear_dir`).
pub const GEAR_DIR_NAME: &str = "gear.d";

/// Resolve the gear drop-in directory for an env.
pub fn gear_dir(env_dir: &Path) -> PathBuf {
    env_dir.join(GEAR_DIR_NAME)
}

/// Filename a given cookbook should write into `gear.d/`. Stable so the
/// reader and writer agree on per-cookbook ownership of the file.
pub fn gear_filename(cookbook_name: &str) -> String {
    format!("cookbook-{cookbook_name}.json")
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GearFile {
    pub version: u32,
    pub gear: HashMap<String, Gear>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Gear {
    pub description: String,
    #[serde(default)]
    pub web: HashMap<String, WebEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebEntry {
    pub description: String,
    pub url: String,
}

/// Read every `*.json` file in `<env_dir>/gear.d/` in lexicographic order,
/// deserialize each into a `GearFile`, and merge their `gear` maps into one.
///
/// Returns an empty map if `gear.d/` does not exist. Files that fail to parse
/// are logged to stderr and skipped — one bad file does not prevent the rest
/// from loading. A gear name appearing in two files is a hard error.
pub fn read_gear_dir(env_dir: &Path) -> anyhow::Result<HashMap<String, Gear>> {
    let dir = gear_dir(env_dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("Could not read gear directory {}", dir.display()));
        }
    };

    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();

    let mut merged: HashMap<String, Gear> = HashMap::new();
    let mut sources: HashMap<String, String> = HashMap::new();

    for path in paths {
        let file_label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());

        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(err) => {
                eprintln!("warning: could not read {}: {}", path.display(), err);
                continue;
            }
        };

        let parsed: GearFile = match serde_json::from_str(&contents) {
            Ok(g) => g,
            Err(err) => {
                eprintln!("warning: could not parse {}: {}", path.display(), err);
                continue;
            }
        };

        for (name, gear) in parsed.gear {
            if let Some(other) = sources.get(&name) {
                bail!(
                    "Gear name '{}' is defined in both {} and {}",
                    name,
                    other,
                    file_label
                );
            }
            sources.insert(name.clone(), file_label.clone());
            merged.insert(name, gear);
        }
    }

    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::{Gear, GearFile, SCHEMA_VERSION, WebEntry, gear_dir, read_gear_dir};
    use std::fs;

    /// Sample valid JSON document conforming to the gear schema.
    fn valid_full_schema_json() -> &'static str {
        r#"{
            "version": 1,
            "gear": {
                "pr": {
                    "description": "Pull request #309 on kantord/enwiro",
                    "web": {
                        "page": {
                            "description": "Open the PR page",
                            "url": "https://github.com/kantord/enwiro/pull/309"
                        }
                    }
                }
            }
        }"#
    }

    #[test]
    fn deserializes_valid_full_schema_into_gear_file() {
        let parsed: GearFile = serde_json::from_str(valid_full_schema_json())
            .expect("valid schema must deserialize successfully");

        assert_eq!(parsed.version, 1, "version field must round-trip as 1");
        assert_eq!(
            parsed.gear.len(),
            1,
            "expected exactly one entry in the gear map"
        );

        let pr_gear: &Gear = parsed.gear.get("pr").expect("`pr` gear must be present");
        assert_eq!(pr_gear.description, "Pull request #309 on kantord/enwiro");
        assert_eq!(
            pr_gear.web.len(),
            1,
            "expected exactly one entry in the web map"
        );

        let page: &WebEntry = pr_gear
            .web
            .get("page")
            .expect("`page` web entry must be present");
        assert_eq!(page.description, "Open the PR page");
        assert_eq!(page.url, "https://github.com/kantord/enwiro/pull/309");
    }

    #[test]
    fn errors_when_top_level_version_is_missing() {
        let json = r#"{
            "gear": {
                "pr": {
                    "description": "x",
                    "web": {}
                }
            }
        }"#;

        let result: Result<GearFile, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "missing top-level `version` must fail to deserialize, got: {result:?}"
        );
    }

    #[test]
    fn errors_when_top_level_gear_is_missing() {
        let json = r#"{ "version": 1 }"#;

        let result: Result<GearFile, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "missing top-level `gear` must fail to deserialize, got: {result:?}"
        );
    }

    #[test]
    fn errors_when_gear_entry_has_no_description() {
        let json = r#"{
            "version": 1,
            "gear": {
                "pr": {
                    "web": {}
                }
            }
        }"#;

        let result: Result<GearFile, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "gear entry without `description` must fail to deserialize, got: {result:?}"
        );
    }

    #[test]
    fn gear_entry_without_web_field_succeeds_with_empty_web_map() {
        let json = r#"{
            "version": 1,
            "gear": {
                "cli-only": {
                    "description": "A gear that has no web entries yet"
                }
            }
        }"#;

        let parsed: GearFile = serde_json::from_str(json)
            .expect("missing `web` should default to empty map, not error");
        let cli_only = parsed
            .gear
            .get("cli-only")
            .expect("`cli-only` gear must be present");
        assert!(
            cli_only.web.is_empty(),
            "absent `web` field must default to empty map, got {} entries",
            cli_only.web.len()
        );
    }

    #[test]
    fn errors_when_web_entry_has_no_url() {
        let json = r#"{
            "version": 1,
            "gear": {
                "pr": {
                    "description": "x",
                    "web": {
                        "page": {
                            "description": "Open the page"
                        }
                    }
                }
            }
        }"#;

        let result: Result<GearFile, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "web entry without `url` must fail to deserialize, got: {result:?}"
        );
    }

    #[test]
    fn errors_when_web_entry_has_no_description() {
        let json = r#"{
            "version": 1,
            "gear": {
                "pr": {
                    "description": "x",
                    "web": {
                        "page": {
                            "url": "https://example.com"
                        }
                    }
                }
            }
        }"#;

        let result: Result<GearFile, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "web entry without `description` must fail to deserialize, got: {result:?}"
        );
    }

    #[test]
    fn errors_on_unknown_field_at_top_level() {
        let json = r#"{
            "version": 1,
            "gear": {},
            "extra_top_level": true
        }"#;

        let result: Result<GearFile, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "unknown top-level field must fail (`deny_unknown_fields`), got: {result:?}"
        );
    }

    #[test]
    fn errors_on_unknown_field_inside_gear_entry() {
        let json = r#"{
            "version": 1,
            "gear": {
                "pr": {
                    "description": "x",
                    "web": {},
                    "rogue": 42
                }
            }
        }"#;

        let result: Result<GearFile, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "unknown field inside a gear entry must fail (`deny_unknown_fields`), got: {result:?}"
        );
    }

    #[test]
    fn errors_on_unknown_field_inside_web_entry() {
        let json = r#"{
            "version": 1,
            "gear": {
                "pr": {
                    "description": "x",
                    "web": {
                        "page": {
                            "description": "Open the page",
                            "url": "https://example.com",
                            "rogue": "value"
                        }
                    }
                }
            }
        }"#;

        let result: Result<GearFile, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "unknown field inside a web entry must fail (`deny_unknown_fields`), got: {result:?}"
        );
    }

    fn write_gear_file(env_dir: &std::path::Path, file_name: &str, gears_json: &str) {
        let dir = gear_dir(env_dir);
        fs::create_dir_all(&dir).unwrap();
        let body = format!(r#"{{"version": {SCHEMA_VERSION}, "gear": {gears_json}}}"#);
        fs::write(dir.join(file_name), body).unwrap();
    }

    fn one_gear_json(name: &str, description: &str) -> String {
        format!(
            r#"{{
                "{name}": {{
                    "description": "{description}",
                    "web": {{
                        "page": {{
                            "description": "Open it",
                            "url": "https://example.com/{name}"
                        }}
                    }}
                }}
            }}"#
        )
    }

    #[test]
    fn read_gear_dir_returns_empty_when_directory_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let result = read_gear_dir(tmp.path()).unwrap();
        assert!(result.is_empty(), "missing gear.d/ must yield empty map");
    }

    #[test]
    fn read_gear_dir_loads_single_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_gear_file(
            tmp.path(),
            "cookbook-github.json",
            &one_gear_json("pr", "PR #1"),
        );

        let result = read_gear_dir(tmp.path()).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result["pr"].description, "PR #1");
        assert_eq!(result["pr"].web["page"].url, "https://example.com/pr");
    }

    #[test]
    fn read_gear_dir_merges_distinct_gears_across_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_gear_file(
            tmp.path(),
            "cookbook-github.json",
            &one_gear_json("pr", "PR"),
        );
        write_gear_file(tmp.path(), "user.json", &one_gear_json("notes", "Notes"));

        let result = read_gear_dir(tmp.path()).unwrap();

        assert_eq!(result.len(), 2);
        assert!(result.contains_key("pr"));
        assert!(result.contains_key("notes"));
    }

    #[test]
    fn read_gear_dir_errors_on_gear_name_collision_across_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_gear_file(
            tmp.path(),
            "a-cookbook.json",
            &one_gear_json("pr", "from a"),
        );
        write_gear_file(
            tmp.path(),
            "z-cookbook.json",
            &one_gear_json("pr", "from z"),
        );

        let err = read_gear_dir(tmp.path()).expect_err("collision must be an error");
        let msg = format!("{err:#}");

        assert!(
            msg.contains("'pr'"),
            "error must name the colliding gear: {msg}"
        );
        assert!(
            msg.contains("a-cookbook.json"),
            "error must mention the first source file (sorted): {msg}"
        );
        assert!(
            msg.contains("z-cookbook.json"),
            "error must mention the second source file: {msg}"
        );
    }

    #[test]
    fn read_gear_dir_skips_malformed_files_and_loads_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = gear_dir(tmp.path());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("broken.json"), "{not valid json").unwrap();
        write_gear_file(
            tmp.path(),
            "cookbook-github.json",
            &one_gear_json("pr", "PR"),
        );

        let result = read_gear_dir(tmp.path()).unwrap();

        assert_eq!(result.len(), 1, "one good file must still be loaded");
        assert!(result.contains_key("pr"));
    }

    #[test]
    fn read_gear_dir_ignores_non_json_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = gear_dir(tmp.path());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("README.md"), "this is not gear").unwrap();
        write_gear_file(
            tmp.path(),
            "cookbook-github.json",
            &one_gear_json("pr", "PR"),
        );

        let result = read_gear_dir(tmp.path()).unwrap();

        assert_eq!(result.len(), 1);
    }
}
