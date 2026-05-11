use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use enwiro_sdk::{CookbookMetadata, Recipe};
use serde::Deserialize;

const RECIPE_PREFIX: &str = "obsidian#";
const DEFAULT_PRIORITY: u32 = 40;
const OBSIDIAN_BINARY: &str = "obsidian";
const ZOTERO_BINARY: &str = "zotero";

#[derive(Debug, Deserialize)]
struct ObsidianVaultEntry {
    path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct ObsidianJson {
    vaults: HashMap<String, ObsidianVaultEntry>,
}

struct VaultRecipe {
    recipe: Recipe,
    vault_path: PathBuf,
}

fn slugify(dir_name: &str) -> String {
    dir_name.to_lowercase().replace(' ', "-")
}

fn compute_sort_order(index: usize, total: usize) -> u32 {
    if total <= 1 {
        0
    } else {
        ((index * 100) / (total - 1)) as u32
    }
}

fn vault_dir_name(path: &Path) -> Option<String> {
    Some(path.file_name()?.to_string_lossy().into_owned())
}

fn vault_recipes_from_json(json: &str) -> Result<Vec<VaultRecipe>> {
    let parsed: ObsidianJson =
        serde_json::from_str(json).context("Failed to parse obsidian.json")?;

    let mut by_slug: BTreeMap<String, Vec<(PathBuf, String)>> = BTreeMap::new();
    for vault in parsed.vaults.into_values() {
        let Some(dir_name) = vault_dir_name(&vault.path) else {
            continue;
        };
        by_slug
            .entry(slugify(&dir_name))
            .or_default()
            .push((vault.path, dir_name));
    }

    let mut named: Vec<(String, String, PathBuf)> = Vec::new();
    for (slug, mut group) in by_slug {
        group.sort_by(|a, b| a.0.cmp(&b.0));
        let collides = group.len() > 1;
        for (i, (path, dir_name)) in group.into_iter().enumerate() {
            let name = if collides {
                format!("{RECIPE_PREFIX}{slug}-{}", i + 1)
            } else {
                format!("{RECIPE_PREFIX}{slug}")
            };
            let description = if collides {
                path.display().to_string()
            } else {
                dir_name
            };
            named.push((name, description, path));
        }
    }

    let total = named.len();
    Ok(named
        .into_iter()
        .enumerate()
        .map(|(index, (name, description, vault_path))| VaultRecipe {
            recipe: Recipe {
                name,
                description: Some(description),
                sort_order: compute_sort_order(index, total),
            },
            vault_path,
        })
        .collect())
}

fn list_recipes_from_json(json: &str) -> Result<Vec<Recipe>> {
    Ok(vault_recipes_from_json(json)?
        .into_iter()
        .map(|vr| vr.recipe)
        .collect())
}

fn cook_from_json(recipe_name: &str, json: &str) -> Result<PathBuf> {
    vault_recipes_from_json(json)?
        .into_iter()
        .find(|vr| vr.recipe.name == recipe_name)
        .map(|vr| vr.vault_path)
        .ok_or_else(|| anyhow!("No vault found matching recipe '{}'", recipe_name))
}

/// Build a single-app gear with one `linux-gui` entry. Used to assemble the
/// per-app gears (obsidian, zotero) that this cookbook emits.
fn single_app_gear(description: &str, binary: &str) -> enwiro_sdk::gear::Gear {
    use enwiro_sdk::gear::{Gear, GuiEntry};
    Gear {
        description: description.to_string(),
        linux_gui: HashMap::from([(
            "app".to_string(),
            GuiEntry {
                command: vec![binary.to_string()],
            },
        )]),
        ..Default::default()
    }
}

/// Build the gear file this cookbook emits. Each app is included only when
/// its binary is on `PATH` at gear-emit time; the i3 adapter also re-checks
/// at activation time so a stale gear file (binary removed after emission)
/// is handled gracefully.
fn build_gear_file(
    obsidian_available: bool,
    zotero_available: bool,
) -> enwiro_sdk::gear::GearFileData {
    use enwiro_sdk::gear::{GearFileData, SCHEMA_VERSION};

    let mut gear = HashMap::new();
    if obsidian_available {
        gear.insert(
            "obsidian".to_string(),
            single_app_gear("Obsidian", OBSIDIAN_BINARY),
        );
    }
    if zotero_available {
        gear.insert(
            "zotero".to_string(),
            single_app_gear("Zotero", ZOTERO_BINARY),
        );
    }
    GearFileData {
        version: SCHEMA_VERSION,
        gear,
    }
}

/// Validate the recipe and write the gear JSON to `writer`. The two
/// `_available` flags are injected so tests can exercise every gating
/// combination without depending on the host `PATH`; the CLI handler in
/// `cmd_gear` passes the result of `which::which`.
fn gear_from_json<W: std::io::Write>(
    recipe_name: &str,
    json: &str,
    obsidian_available: bool,
    zotero_available: bool,
    writer: &mut W,
) -> Result<()> {
    // Recipe validation reuses cook_from_json: if the slug doesn't match a
    // known vault, the user got the name wrong and we shouldn't emit anything.
    cook_from_json(recipe_name, json)?;

    let file = build_gear_file(obsidian_available, zotero_available);
    serde_json::to_writer(writer, &file).context("Failed to serialize gear")?;
    Ok(())
}

#[derive(Parser)]
enum EnwiroCookbookObsidian {
    ListRecipes(ListRecipesArgs),
    Cook(CookArgs),
    Gear(GearArgs),
    Metadata,
}

#[derive(clap::Args)]
pub struct ListRecipesArgs {}

#[derive(clap::Args)]
pub struct CookArgs {
    recipe_name: String,
}

#[derive(clap::Args)]
pub struct GearArgs {
    recipe_name: String,
}

fn linux_obsidian_json_path() -> Result<PathBuf> {
    let home = home::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".config").join("obsidian").join("obsidian.json"))
}

fn read_obsidian_json() -> Result<String> {
    let path = linux_obsidian_json_path()?;
    std::fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))
}

fn cmd_list_recipes() -> Result<()> {
    let json = read_obsidian_json()?;
    for recipe in list_recipes_from_json(&json)? {
        println!("{}", recipe.to_jsonl());
    }
    Ok(())
}

fn cmd_cook(recipe_name: &str) -> Result<()> {
    let json = read_obsidian_json()?;
    let path = cook_from_json(recipe_name, &json)?;
    print!("{}", path.display());
    Ok(())
}

fn cmd_gear(recipe_name: &str) -> Result<()> {
    let json = read_obsidian_json()?;
    // The cookbook-side `which` checks suppress emit-time entries for apps
    // that aren't installed - keeps the gear file clean and avoids
    // activation-time warnings. The i3 adapter ALSO checks `which` before
    // spawning each command (see `enwiro-adapter-i3wm::spawn_gui_commands`):
    // that's the real safety net for stale gear files where a binary was
    // installed at emit time but removed before activation. Both layers are
    // intentional, not redundant.
    let obsidian_available = which::which(OBSIDIAN_BINARY).is_ok();
    let zotero_available = which::which(ZOTERO_BINARY).is_ok();
    gear_from_json(
        recipe_name,
        &json,
        obsidian_available,
        zotero_available,
        &mut std::io::stdout(),
    )
}

fn main() -> Result<()> {
    let _guard = enwiro_sdk::init_logging("enwiro-cookbook-obsidian.log");

    let args = EnwiroCookbookObsidian::parse();

    match args {
        EnwiroCookbookObsidian::ListRecipes(_) => cmd_list_recipes()?,
        EnwiroCookbookObsidian::Cook(a) => cmd_cook(&a.recipe_name)?,
        EnwiroCookbookObsidian::Gear(a) => cmd_gear(&a.recipe_name)?,
        EnwiroCookbookObsidian::Metadata => {
            println!(
                "{}",
                CookbookMetadata {
                    default_priority: Some(DEFAULT_PRIORITY)
                }
                .to_json()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault_json_for_paths(paths: &[&str]) -> String {
        let entries: Vec<String> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| {
                format!(
                    r#""key{}":{{"path":"{}","ts":{}}}"#,
                    i,
                    p,
                    1_700_000_000_000_u64 + i as u64
                )
            })
            .collect();
        format!(r#"{{"vaults":{{{}}}}}"#, entries.join(","))
    }

    fn single_vault_json(dir_name: &str) -> String {
        vault_json_for_paths(&[&format!("/home/user/{dir_name}")])
    }

    fn two_vault_json() -> String {
        vault_json_for_paths(&["/home/user/My notes", "/home/user/Work Vault"])
    }

    mod slugify {
        fn check(input: &str, expected: &str) {
            assert_eq!(super::super::slugify(input), expected);
        }

        #[test]
        fn lowercases() {
            check("MyNotes", "mynotes");
        }

        #[test]
        fn replaces_spaces_with_hyphens() {
            check("My notes", "my-notes");
        }

        #[test]
        fn handles_multiple_spaces() {
            check("A B C", "a-b-c");
        }
    }

    mod list_recipes {
        use super::*;

        fn check_name(dir_name: &str, expected: &str) {
            let recipes = list_recipes_from_json(&single_vault_json(dir_name)).unwrap();
            assert_eq!(recipes[0].name, expected);
        }

        #[test]
        fn single_vault_produces_one_recipe() {
            let recipes = list_recipes_from_json(&single_vault_json("mynotes")).unwrap();
            assert_eq!(recipes.len(), 1);
        }

        #[test]
        fn two_vaults_produce_two_recipes() {
            let recipes = list_recipes_from_json(&two_vault_json()).unwrap();
            assert_eq!(recipes.len(), 2);
        }

        #[test]
        fn name_uses_obsidian_prefix() {
            check_name("mynotes", "obsidian#mynotes");
        }

        #[test]
        fn name_lowercases_uppercase() {
            check_name("MyNotes", "obsidian#mynotes");
        }

        #[test]
        fn name_replaces_spaces() {
            check_name("My notes", "obsidian#my-notes");
        }

        #[test]
        fn description_is_original_dir_name() {
            let recipes = list_recipes_from_json(&single_vault_json("My notes")).unwrap();
            assert_eq!(recipes[0].description.as_deref(), Some("My notes"));
        }

        #[test]
        fn description_preserves_case() {
            let recipes = list_recipes_from_json(&single_vault_json("Work Vault")).unwrap();
            assert_eq!(recipes[0].description.as_deref(), Some("Work Vault"));
        }

        #[test]
        fn two_vaults_have_correct_names_and_descriptions() {
            let recipes = list_recipes_from_json(&two_vault_json()).unwrap();
            let by_name: HashMap<&str, &Recipe> =
                recipes.iter().map(|r| (r.name.as_str(), r)).collect();
            assert_eq!(
                by_name["obsidian#my-notes"].description.as_deref(),
                Some("My notes")
            );
            assert_eq!(
                by_name["obsidian#work-vault"].description.as_deref(),
                Some("Work Vault")
            );
        }

        #[test]
        fn invalid_json_returns_error() {
            assert!(list_recipes_from_json("not json").is_err());
        }

        #[test]
        fn trailing_slash_path_still_produces_recipe() {
            let json = vault_json_for_paths(&["/home/user/My notes/"]);
            let recipes = list_recipes_from_json(&json).unwrap();
            assert_eq!(recipes.len(), 1);
            assert_eq!(recipes[0].name, "obsidian#my-notes");
        }

        #[test]
        fn empty_vaults_returns_zero_recipes() {
            let recipes = list_recipes_from_json(r#"{"vaults":{}}"#).unwrap();
            assert!(recipes.is_empty());
        }

        #[test]
        fn vault_with_no_basename_is_skipped() {
            let json = vault_json_for_paths(&["/"]);
            let recipes = list_recipes_from_json(&json).unwrap();
            assert!(recipes.is_empty());
        }

        #[test]
        fn order_is_deterministic_across_calls() {
            let json = vault_json_for_paths(&[
                "/v/a", "/v/b", "/v/c", "/v/d", "/v/e", "/v/f", "/v/g", "/v/h",
            ]);
            let names = |json: &str| -> Vec<String> {
                list_recipes_from_json(json)
                    .unwrap()
                    .into_iter()
                    .map(|r| r.name)
                    .collect()
            };
            let first = names(&json);
            for _ in 0..5 {
                assert_eq!(names(&json), first);
            }
        }

        #[test]
        fn assigns_linear_sort_order() {
            let json = vault_json_for_paths(&["/v/a", "/v/b", "/v/c", "/v/d", "/v/e"]);
            let orders: Vec<u32> = list_recipes_from_json(&json)
                .unwrap()
                .iter()
                .map(|r| r.sort_order)
                .collect();
            assert_eq!(orders, vec![0, 25, 50, 75, 100]);
        }

        #[test]
        fn single_recipe_gets_sort_order_zero() {
            let recipes = list_recipes_from_json(&single_vault_json("only")).unwrap();
            assert_eq!(recipes[0].sort_order, 0);
        }

        #[test]
        fn colliding_dir_names_produce_distinct_recipes() {
            let json =
                vault_json_for_paths(&["/home/user/work/Notes", "/home/user/personal/Notes"]);
            let recipes = list_recipes_from_json(&json).unwrap();
            assert_eq!(recipes.len(), 2);
            let names: std::collections::HashSet<&str> =
                recipes.iter().map(|r| r.name.as_str()).collect();
            assert_eq!(names.len(), 2);
        }

        #[test]
        fn collided_recipe_descriptions_show_full_path() {
            let json =
                vault_json_for_paths(&["/home/user/work/Notes", "/home/user/personal/Notes"]);
            let descriptions: Vec<String> = list_recipes_from_json(&json)
                .unwrap()
                .into_iter()
                .filter_map(|r| r.description)
                .collect();
            assert!(descriptions.iter().any(|d| d == "/home/user/work/Notes"));
            assert!(
                descriptions
                    .iter()
                    .any(|d| d == "/home/user/personal/Notes")
            );
        }
    }

    mod cook {
        use super::*;

        fn check_match(recipe_name: &str, dir_name: &str, expected: &str) {
            let json = single_vault_json(dir_name);
            let result = cook_from_json(recipe_name, &json).unwrap();
            assert_eq!(result, PathBuf::from(expected));
        }

        #[test]
        fn matches_simple_vault() {
            check_match("obsidian#mynotes", "mynotes", "/home/user/mynotes");
        }

        #[test]
        fn matches_slugified_spaces() {
            check_match("obsidian#my-notes", "My notes", "/home/user/My notes");
        }

        #[test]
        fn matches_slugified_uppercase() {
            check_match("obsidian#mynotes", "MyNotes", "/home/user/MyNotes");
        }

        #[test]
        fn selects_correct_vault_among_multiple() {
            let result = cook_from_json("obsidian#work-vault", &two_vault_json()).unwrap();
            assert_eq!(result, PathBuf::from("/home/user/Work Vault"));
        }

        #[test]
        fn returns_error_when_no_vault_matches() {
            let result = cook_from_json("obsidian#nonexistent", &single_vault_json("mynotes"));
            assert!(result.is_err());
        }

        #[test]
        fn returns_error_for_empty_vaults() {
            let result = cook_from_json("obsidian#anything", r#"{"vaults":{}}"#);
            assert!(result.is_err());
        }

        #[test]
        fn does_not_match_vault_with_no_basename() {
            let json = vault_json_for_paths(&["/"]);
            let result = cook_from_json("obsidian#", &json);
            assert!(result.is_err());
        }

        #[test]
        fn resolves_collided_slug_to_specific_vault() {
            let json =
                vault_json_for_paths(&["/home/user/work/Notes", "/home/user/personal/Notes"]);
            let recipes = list_recipes_from_json(&json).unwrap();
            for recipe in recipes {
                let path = cook_from_json(&recipe.name, &json).unwrap();
                assert!(
                    path == PathBuf::from("/home/user/work/Notes")
                        || path == PathBuf::from("/home/user/personal/Notes"),
                );
            }
        }
    }

    mod gear {
        use super::*;

        /// Run `gear_from_json` with one vault registered and assert the
        /// emitted JSON matches `expected`. Centralizes the boilerplate
        /// (output buffer, recipe name, fixture JSON) so individual tests
        /// stay declarative.
        fn assert_gear_emits(
            obsidian_available: bool,
            zotero_available: bool,
            expected: serde_json::Value,
        ) {
            let json = single_vault_json("mynotes");
            let mut output = Vec::new();
            gear_from_json(
                "obsidian#mynotes",
                &json,
                obsidian_available,
                zotero_available,
                &mut output,
            )
            .unwrap();
            let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(parsed, expected);
        }

        #[test]
        fn emits_obsidian_only_when_zotero_missing() {
            assert_gear_emits(
                true,
                false,
                serde_json::json!({
                    "version": 1,
                    "gear": {
                        "obsidian": {
                            "description": "Obsidian",
                            "linux-gui": {
                                "app": { "command": ["obsidian"] }
                            }
                        }
                    }
                }),
            );
        }

        #[test]
        fn emits_zotero_only_when_obsidian_missing() {
            assert_gear_emits(
                false,
                true,
                serde_json::json!({
                    "version": 1,
                    "gear": {
                        "zotero": {
                            "description": "Zotero",
                            "linux-gui": {
                                "app": { "command": ["zotero"] }
                            }
                        }
                    }
                }),
            );
        }

        #[test]
        fn emits_obsidian_and_zotero_when_both_available() {
            assert_gear_emits(
                true,
                true,
                serde_json::json!({
                    "version": 1,
                    "gear": {
                        "obsidian": {
                            "description": "Obsidian",
                            "linux-gui": {
                                "app": { "command": ["obsidian"] }
                            }
                        },
                        "zotero": {
                            "description": "Zotero",
                            "linux-gui": {
                                "app": { "command": ["zotero"] }
                            }
                        }
                    }
                }),
            );
        }

        #[test]
        fn emits_empty_gear_when_neither_available() {
            assert_gear_emits(
                false,
                false,
                serde_json::json!({
                    "version": 1,
                    "gear": {}
                }),
            );
        }

        #[test]
        fn returns_error_when_recipe_unknown() {
            let json = single_vault_json("mynotes");
            let mut output = Vec::new();
            let result = gear_from_json("obsidian#nonexistent", &json, true, true, &mut output);
            assert!(
                result.is_err(),
                "gear emission must fail for an unknown recipe",
            );
            assert!(
                output.is_empty(),
                "no JSON should be emitted when validation fails",
            );
        }
    }
}
