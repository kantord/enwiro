use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Wire version of the gear schema. Bumped on **breaking** wire changes
/// only — additive optional fields don't bump (readers default them via
/// serde, producers can omit). Cookbook authors set
/// `GearFileData { version: SCHEMA_VERSION, ... }` to track upgrades.
///
/// All gear wire structs use `#[serde(rename_all = "kebab-case")]`, so
/// multi-word fields serialize with hyphens (`linux_gui` → `linux-gui`).
pub const SCHEMA_VERSION: u32 = 1;

/// Subdirectory inside an env where gear files live. Each cookbook drops
/// its contribution as a single file under this directory; the reader
/// merges them gear-atomically (see [`LoadedGear::from_env_dir`]).
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

/// Reject unknown versions with a clear message instead of falling
/// through to misleading "missing field X" errors.
fn deserialize_supported_version<'de, D: serde::Deserializer<'de>>(de: D) -> Result<u32, D::Error> {
    let v = u32::deserialize(de)?;
    if v == SCHEMA_VERSION {
        Ok(v)
    } else {
        Err(serde::de::Error::custom(format!(
            "unsupported gear schema version {v}; this build handles version {SCHEMA_VERSION}"
        )))
    }
}

/// Wire format: the JSON contents of one `gear.d/cookbook-X.json` file.
/// Cookbooks construct this and serialize it to stdout; readers
/// deserialize it via serde. Does not carry runtime metadata like the
/// file path - see `GearFile` for the on-disk-loaded wrapper.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct GearFileData {
    #[serde(deserialize_with = "deserialize_supported_version")]
    pub version: u32,
    pub gear: HashMap<String, Gear>,
}

/// A gear file as loaded from disk: its path on the filesystem paired
/// with the parsed contents. The two bundle because the path is needed
/// for diagnostics (collision error messages, debug logs) and travels
/// naturally with the data once we've read it.
pub struct GearFile {
    pub path: PathBuf,
    pub data: GearFileData,
}

impl GearFile {
    /// Read and parse a single gear file from disk. The path is preserved
    /// alongside the parsed data so callers can produce useful diagnostics
    /// (e.g. collision errors that name both source files).
    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Could not read {}", path.display()))?;
        let data: GearFileData = serde_json::from_str(&contents)
            .with_context(|| format!("Could not parse {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            data,
        })
    }

    /// Short label suitable for error messages and logs: the filename
    /// component, falling back to the full display path when the filename
    /// is missing or non-UTF-8.
    pub fn label(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Gear {
    pub description: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub web: HashMap<String, WebEntry>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub linux_gui: HashMap<String, GuiEntry>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub cli: HashMap<String, CliEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct WebEntry {
    pub description: String,
    pub url: String,
}

/// A native Linux GUI app entry inside a [`Gear`]. `command` is the argv
/// to spawn (e.g. `["obsidian"]`); `command[0]` is the binary name.
///
/// Assumed to be a single-instance app: re-spawning a running app is
/// typically a no-op or raises the existing window, so callers may safely
/// fire-and-forget without deduplicating themselves.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct GuiEntry {
    pub command: Vec<String>,
}

/// A user-invokable CLI entry inside a [`Gear`]. `command` is the argv to
/// exec (e.g. `["just", "build"]`); `description` is optional because
/// upstream sources (justfile recipes without doc comments, etc.) may not
/// provide one. The dispatcher (`enw :<gear> <entry>`) resolves the entry
/// and execs `command` followed by any user-supplied passthrough args.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CliEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub command: Vec<String>,
}

/// All gear collected for an env, merged across every `gear.d/*.json`
/// file. The merge is gear-atomic: a gear name appearing in two files
/// is a hard error naming both source files.
///
/// Constructed via [`LoadedGear::from_env_dir`]. Cookbooks emit
/// [`GearFileData`] (the wire-format type); the loader pairs each parsed
/// file with its path via [`GearFile`] and produces a `LoadedGear`.
pub struct LoadedGear {
    gear: HashMap<String, Gear>,
}

impl LoadedGear {
    /// Read every `*.json` file in `<env_dir>/gear.d/` in lexicographic
    /// order, parse each via [`GearFile::from_path`], and merge their
    /// gear maps into one.
    ///
    /// Returns an empty `LoadedGear` if `gear.d/` does not exist. Files
    /// that fail to read or parse are logged at `WARN` and skipped -
    /// one bad file does not prevent the rest from loading. A gear name
    /// appearing in two files is a hard error.
    pub fn from_env_dir(env_dir: &Path) -> anyhow::Result<Self> {
        let dir = gear_dir(env_dir);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    gear: HashMap::new(),
                });
            }
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
            let file = match GearFile::from_path(&path) {
                Ok(f) => f,
                Err(err) => {
                    warn!(error = %format!("{err:#}"), "Skipping gear file");
                    continue;
                }
            };
            let label = file.label();
            for (name, gear) in file.data.gear {
                if let Some(prior) = sources.get(&name) {
                    bail!(
                        "Gear name '{}' is defined in both {} and {}",
                        name,
                        prior,
                        label
                    );
                }
                sources.insert(name.clone(), label.clone());
                merged.insert(name, gear);
            }
        }

        Ok(Self { gear: merged })
    }

    /// Consume the `LoadedGear` and yield the merged map. Callers that
    /// just need the inner `HashMap` (e.g. the activate path passing
    /// gear to the adapter) use this; future callers that want richer
    /// access (`get`, iteration) can use the methods below.
    pub fn into_map(self) -> HashMap<String, Gear> {
        self.gear
    }

    pub fn get(&self, name: &str) -> Option<&Gear> {
        self.gear.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Gear)> {
        self.gear.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.gear.is_empty()
    }
}

#[cfg(test)]
mod tests {
    mod schema {
        use super::super::{CliEntry, Gear, GearFileData, GuiEntry, WebEntry};
        use rstest::rstest;

        /// Sample valid JSON document conforming to the gear schema.
        /// Exercises both entry kinds (web URL and linux-gui command) so
        /// the comprehensive deserialization test covers each.
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
                    },
                    "obsidian": {
                        "description": "Obsidian notes",
                        "linux-gui": {
                            "app": {
                                "command": ["obsidian"]
                            }
                        }
                    }
                }
            }"#
        }

        #[test]
        fn deserializes_valid_full_schema_into_gear_file() {
            let parsed: GearFileData = serde_json::from_str(valid_full_schema_json())
                .expect("valid schema must deserialize successfully");

            assert_eq!(parsed.version, 1, "version field must round-trip as 1");
            assert_eq!(
                parsed.gear.len(),
                2,
                "expected one web gear and one linux-gui gear"
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

            let obsidian_gear: &Gear = parsed
                .gear
                .get("obsidian")
                .expect("`obsidian` gear must be present");
            assert_eq!(obsidian_gear.description, "Obsidian notes");
            assert_eq!(
                obsidian_gear.linux_gui.len(),
                1,
                "expected exactly one entry in the linux-gui map"
            );

            let app: &GuiEntry = obsidian_gear
                .linux_gui
                .get("app")
                .expect("`app` gui entry must be present");
            assert_eq!(app.command, vec!["obsidian"]);
        }

        /// All schema-violation cases share the shape "feed JSON, expect
        /// `Err`". The case label names the violated rule so failures point
        /// at the actual constraint.
        #[rstest]
        #[case::version_missing(r#"{ "gear": { "pr": { "description": "x", "web": {} } } }"#)]
        #[case::gear_missing(r#"{ "version": 1 }"#)]
        #[case::gear_entry_no_description(r#"{ "version": 1, "gear": { "pr": { "web": {} } } }"#)]
        #[case::web_entry_no_url(
            r#"{ "version": 1, "gear": { "pr": { "description": "x",
                "web": { "page": { "description": "Open the page" } } } } }"#
        )]
        #[case::web_entry_no_description(
            r#"{ "version": 1, "gear": { "pr": { "description": "x",
                "web": { "page": { "url": "https://example.com" } } } } }"#
        )]
        #[case::unknown_top_level_field(r#"{ "version": 1, "gear": {}, "extra_top_level": true }"#)]
        #[case::unknown_field_in_gear_entry(
            r#"{ "version": 1, "gear": { "pr": {
                "description": "x", "web": {}, "rogue": 42 } } }"#
        )]
        #[case::unknown_field_in_web_entry(
            r#"{ "version": 1, "gear": { "pr": { "description": "x",
                "web": { "page": { "description": "Open the page",
                    "url": "https://example.com", "rogue": "value" } } } } }"#
        )]
        #[case::gui_entry_no_command(
            r#"{ "version": 1, "gear": { "obsidian": { "description": "x",
                "linux-gui": { "app": {} } } } }"#
        )]
        #[case::unknown_field_in_gui_entry(
            r#"{ "version": 1, "gear": { "obsidian": { "description": "x",
                "linux-gui": { "app": { "command": ["obsidian"], "rogue": 42 } } } } }"#
        )]
        #[case::unsupported_schema_version(r#"{ "version": 999, "gear": {} }"#)]
        #[case::cli_entry_no_command(
            r#"{ "version": 1, "gear": { "just": { "description": "x",
                "cli": { "build": { "description": "Build" } } } } }"#
        )]
        #[case::unknown_field_in_cli_entry(
            r#"{ "version": 1, "gear": { "just": { "description": "x",
                "cli": { "build": { "command": ["just", "build"], "rogue": 1 } } } } }"#
        )]
        fn rejects_invalid_schema(#[case] json: &str) {
            let result: Result<GearFileData, _> = serde_json::from_str(json);
            assert!(result.is_err(), "expected rejection, got: {result:?}");
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

            let parsed: GearFileData = serde_json::from_str(json)
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
        fn deserializes_cli_entries_with_optional_description() {
            let json = r#"{
                "version": 1,
                "gear": {
                    "just": {
                        "description": "Tasks from the project's justfile",
                        "cli": {
                            "build": {
                                "description": "Build the project",
                                "command": ["just", "build"]
                            },
                            "deploy": {
                                "command": ["just", "deploy"]
                            }
                        }
                    }
                }
            }"#;
            let parsed: GearFileData = serde_json::from_str(json).unwrap();
            let cli = &parsed.gear["just"].cli;
            assert_eq!(cli.len(), 2);
            assert_eq!(
                cli["build"].description.as_deref(),
                Some("Build the project")
            );
            assert_eq!(cli["build"].command, vec!["just", "build"]);
            assert!(cli["deploy"].description.is_none());
            assert_eq!(cli["deploy"].command, vec!["just", "deploy"]);
        }

        #[test]
        fn cli_entry_with_none_description_serializes_without_the_field() {
            let entry = CliEntry {
                description: None,
                command: vec!["just".into(), "deploy".into()],
            };
            let json = serde_json::to_string(&entry).expect("CliEntry must serialize");
            assert!(
                !json.contains("description"),
                "None description must be omitted from JSON, got: {json}"
            );
        }

        #[test]
        fn cli_entry_with_some_description_round_trips() {
            let original = CliEntry {
                description: Some("Build the project".into()),
                command: vec!["just".into(), "build".into()],
            };
            let json = serde_json::to_string(&original).unwrap();
            let parsed: CliEntry = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.description, original.description);
            assert_eq!(parsed.command, original.command);
        }

        #[test]
        fn cli_field_defaults_to_empty_when_absent() {
            // Producers that don't emit cli (e.g. existing cookbooks) must
            // still parse — the empty map is the additive-compat path.
            let parsed: GearFileData = serde_json::from_str(valid_full_schema_json()).unwrap();
            for gear in parsed.gear.values() {
                assert!(gear.cli.is_empty());
            }
        }

        #[test]
        fn gear_entry_without_linux_gui_field_succeeds_with_empty_map() {
            let json = r#"{
                "version": 1,
                "gear": {
                    "web-only": {
                        "description": "A gear that has only web entries",
                        "web": {
                            "page": {
                                "description": "Open the page",
                                "url": "https://example.com"
                            }
                        }
                    }
                }
            }"#;

            let parsed: GearFileData = serde_json::from_str(json)
                .expect("missing `linux-gui` should default to empty map, not error");
            let gear = parsed
                .gear
                .get("web-only")
                .expect("`web-only` gear must be present");
            assert!(
                gear.linux_gui.is_empty(),
                "absent `linux-gui` field must default to empty map, got {} entries",
                gear.linux_gui.len()
            );
        }
    }

    mod loaded_gear {
        use super::super::{LoadedGear, SCHEMA_VERSION, gear_dir};
        use std::fs;

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
        fn returns_empty_when_directory_missing() {
            let tmp = tempfile::tempdir().unwrap();
            let result = LoadedGear::from_env_dir(tmp.path())
                .map(LoadedGear::into_map)
                .unwrap();
            assert!(result.is_empty(), "missing gear.d/ must yield empty map");
        }

        #[test]
        fn loads_single_file() {
            let tmp = tempfile::tempdir().unwrap();
            write_gear_file(
                tmp.path(),
                "cookbook-github.json",
                &one_gear_json("pr", "PR #1"),
            );

            let result = LoadedGear::from_env_dir(tmp.path())
                .map(LoadedGear::into_map)
                .unwrap();

            assert_eq!(result.len(), 1);
            assert_eq!(result["pr"].description, "PR #1");
            assert_eq!(result["pr"].web["page"].url, "https://example.com/pr");
        }

        #[test]
        fn merges_distinct_gears_across_files() {
            let tmp = tempfile::tempdir().unwrap();
            write_gear_file(
                tmp.path(),
                "cookbook-github.json",
                &one_gear_json("pr", "PR"),
            );
            write_gear_file(tmp.path(), "user.json", &one_gear_json("notes", "Notes"));

            let result = LoadedGear::from_env_dir(tmp.path())
                .map(LoadedGear::into_map)
                .unwrap();

            assert_eq!(result.len(), 2);
            assert!(result.contains_key("pr"));
            assert!(result.contains_key("notes"));
        }

        #[test]
        fn errors_on_gear_name_collision_across_files() {
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

            let err = LoadedGear::from_env_dir(tmp.path())
                .map(LoadedGear::into_map)
                .expect_err("collision must be an error");
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
        fn skips_malformed_files_and_loads_the_rest() {
            let tmp = tempfile::tempdir().unwrap();
            let dir = gear_dir(tmp.path());
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("broken.json"), "{not valid json").unwrap();
            write_gear_file(
                tmp.path(),
                "cookbook-github.json",
                &one_gear_json("pr", "PR"),
            );

            let result = LoadedGear::from_env_dir(tmp.path())
                .map(LoadedGear::into_map)
                .unwrap();

            assert_eq!(result.len(), 1, "one good file must still be loaded");
            assert!(result.contains_key("pr"));
        }

        #[test]
        fn ignores_non_json_files() {
            let tmp = tempfile::tempdir().unwrap();
            let dir = gear_dir(tmp.path());
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("README.md"), "this is not gear").unwrap();
            write_gear_file(
                tmp.path(),
                "cookbook-github.json",
                &one_gear_json("pr", "PR"),
            );

            let result = LoadedGear::from_env_dir(tmp.path())
                .map(LoadedGear::into_map)
                .unwrap();

            assert_eq!(result.len(), 1);
        }
    }
}
