use anyhow::Context;
use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::path::PathBuf;
use std::process::Command;

#[derive(Deserialize)]
struct EntryStatus {
    #[serde(rename = "type")]
    status_type: String,
    #[serde(default)]
    phase: Option<String>,
}

#[derive(Deserialize)]
struct CacheEntry {
    cookbook: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    sort_order: u32,
    #[serde(default)]
    status: Option<EntryStatus>,
    #[serde(default)]
    scores: Option<serde_json::Value>,
}

fn status_label(entry: &CacheEntry) -> &str {
    let is_env = entry.scores.is_some();
    match &entry.status {
        Some(s) => match s.status_type.as_str() {
            "cooked" => match s.phase.as_deref() {
                Some("active") => "active",
                Some("waiting") => "waiting",
                _ => "ready",
            },
            "done" => "done",
            "evergreen" => "evergreen",
            "uncooked" => "*",
            _ => {
                if is_env {
                    "active"
                } else {
                    ""
                }
            }
        },
        None => {
            if is_env {
                "active"
            } else {
                ""
            }
        }
    }
}

fn sort_tier(entry: &CacheEntry) -> u8 {
    let is_env = entry.scores.is_some();
    if !is_env {
        return 1;
    }
    match &entry.status {
        Some(s) if s.status_type == "done" => 2,
        _ => 0,
    }
}
fn enwiro_bin() -> anyhow::Result<PathBuf> {
    if let Ok(path) = env::var("ENWIRO_BIN") {
        tracing::debug!(path = %path, "Using ENWIRO_BIN env var");
        return Ok(PathBuf::from(path));
    }
    let exe = env::current_exe().context("could not determine own executable path")?;
    let dir = exe.parent().context("executable has no parent directory")?;
    let bin = dir.join("enw");
    tracing::debug!(path = %bin.display(), "Resolved enw binary from exe parent");
    Ok(bin)
}

fn format_entries(input: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut parsed: Vec<(usize, CacheEntry)> = Vec::new();
    for (i, line) in input.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<CacheEntry>(line)
            && seen.insert(entry.name.clone())
        {
            parsed.push((i, entry));
        }
    }
    parsed.sort_by_key(|(i, entry)| (sort_tier(entry), *i));
    parsed
        .iter()
        .map(|(_, entry)| {
            let description = entry.description.as_deref().unwrap_or("");
            let status = status_label(entry);
            format!(
                "{}\t{}\t{}\t{}\0info\x1f{}",
                status, entry.cookbook, entry.name, description, entry.cookbook
            )
        })
        .collect()
}

fn list_entries() -> anyhow::Result<()> {
    tracing::debug!("Listing entries via enwiro ls");
    let output = Command::new(enwiro_bin()?)
        .arg("ls")
        .arg("--json")
        .output()
        .context("Failed to run enwiro ls")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(%stderr, "enwiro ls failed");
        anyhow::bail!("enwiro ls failed: {}", stderr);
    }

    let stdout = String::from_utf8(output.stdout)?;
    for entry in format_entries(&stdout) {
        println!("{}", entry);
    }

    Ok(())
}

fn extract_recipe_name(selection: &str) -> &str {
    let rest = selection
        .split_once('\t')
        .map_or(selection, |(_, rest)| rest);
    let rest = rest.split_once('\t').map_or(rest, |(_, rest)| rest);
    rest.split_once('\t').map_or(rest, |(name, _)| name)
}

fn activate_selection(selection: &str) -> anyhow::Result<()> {
    let recipe_name = extract_recipe_name(selection);
    tracing::debug!(selection = %selection, recipe = %recipe_name, "Activating selection");
    // We intentionally spawn without calling .wait(). This lets the bridge
    // exit immediately so rofi can close, while enwiro activate continues
    // in the background (e.g. cooking an environment from a git recipe may
    // take a while). The child becomes a short-lived zombie until this
    // process exits, at which point init reaps it.
    Command::new(enwiro_bin()?)
        .arg("activate")
        .arg(recipe_name)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .context("Failed to spawn enwiro activate")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_entries_columns() {
        let input = r#"{"cookbook":"git","name":"my-project"}"#;
        let entries = format_entries(input);
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].contains("\tgit\tmy-project\t"),
            "Expected tab-separated columns with cookbook and name, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_rofi_metadata() {
        let input = r#"{"cookbook":"git","name":"my-project"}"#;
        let entries = format_entries(input);
        assert!(
            entries[0].contains("\0info\x1fgit"),
            "Expected rofi info metadata, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_deduplicates_by_name() {
        let input = "{\"cookbook\":\"_\",\"name\":\"my-project\"}\n{\"cookbook\":\"git\",\"name\":\"my-project\"}\n";
        let entries = format_entries(input);
        assert_eq!(
            entries.len(),
            1,
            "Duplicate names should be deduplicated: {:?}",
            entries
        );
    }

    #[test]
    fn test_format_entries_keeps_first_source_on_duplicate() {
        let input = "{\"cookbook\":\"_\",\"name\":\"my-project\"}\n{\"cookbook\":\"git\",\"name\":\"my-project\"}\n";
        let entries = format_entries(input);
        assert!(
            entries[0].contains("\t_\tmy-project"),
            "First occurrence should win, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_skips_empty_lines() {
        let input = "\n  \n{\"cookbook\":\"git\",\"name\":\"my-project\"}\n\n";
        let entries = format_entries(input);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_extract_recipe_name_strips_columns() {
        assert_eq!(
            extract_recipe_name("active\tgit\tmy-project\t"),
            "my-project"
        );
    }

    #[test]
    fn test_extract_recipe_name_strips_all_columns() {
        assert_eq!(
            extract_recipe_name("active\tgithub\towner/repo#42\tFix auth bug"),
            "owner/repo#42"
        );
    }

    #[test]
    fn test_extract_recipe_name_without_tab() {
        assert_eq!(extract_recipe_name("my-project"), "my-project");
    }

    #[test]
    fn test_format_entries_multiple_recipes() {
        let input = "{\"cookbook\":\"git\",\"name\":\"project-a\"}\n{\"cookbook\":\"chezmoi\",\"name\":\"chezmoi\"}\n{\"cookbook\":\"git\",\"name\":\"project-b\"}\n";
        let entries = format_entries(input);
        assert_eq!(entries.len(), 3);
        assert!(entries[0].contains("\tgit\tproject-a\t"));
        assert!(entries[1].contains("\tchezmoi\tchezmoi\t"));
        assert!(entries[2].contains("\tgit\tproject-b\t"));
    }

    #[test]
    fn test_format_entries_with_description() {
        let input = r#"{"cookbook":"github","name":"owner/repo#42","description":"Fix auth bug"}"#;
        let entries = format_entries(input);
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].contains("\tgithub\towner/repo#42\tFix auth bug"),
            "Expected description column, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_without_description_has_empty_column() {
        let input = r#"{"cookbook":"git","name":"my-project"}"#;
        let entries = format_entries(input);
        assert!(
            entries[0].contains("\tgit\tmy-project\t\0"),
            "Expected empty description column, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_deduplicates_by_name_ignoring_description() {
        let input = "{\"cookbook\":\"_\",\"name\":\"foo\"}\n{\"cookbook\":\"git\",\"name\":\"foo\",\"description\":\"some description\"}\n";
        let entries = format_entries(input);
        assert_eq!(
            entries.len(),
            1,
            "Should deduplicate by name: {:?}",
            entries
        );
    }

    #[test]
    fn test_format_entries_env_without_status_falls_back_to_active() {
        let input = r#"{"cookbook":"git","name":"proj","scores":{"launcher":0.5,"slot":0.1}}"#;
        let entries = format_entries(input);
        assert!(
            entries[0].starts_with("active\t"),
            "Env without status should fall back to active (legacy env with work in progress), got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_env_active_status() {
        let input = r#"{"cookbook":"git","name":"proj","status":{"type":"cooked","phase":"active"},"scores":{"launcher":0.5,"slot":0.1}}"#;
        let entries = format_entries(input);
        assert!(
            entries[0].starts_with("active\t"),
            "Expected active status, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_env_waiting_status() {
        let input = r#"{"cookbook":"git","name":"proj","status":{"type":"cooked","phase":"waiting"},"scores":{"launcher":0.5,"slot":0.1}}"#;
        let entries = format_entries(input);
        assert!(
            entries[0].starts_with("waiting\t"),
            "Expected waiting status, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_env_ready_status() {
        let input = r#"{"cookbook":"git","name":"proj","status":{"type":"cooked"},"scores":{"launcher":0.5,"slot":0.1}}"#;
        let entries = format_entries(input);
        assert!(
            entries[0].starts_with("ready\t"),
            "Expected ready status, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_env_done_status() {
        let input = r#"{"cookbook":"git","name":"proj","status":{"type":"done"},"scores":{"launcher":0.5,"slot":0.1}}"#;
        let entries = format_entries(input);
        assert!(
            entries[0].starts_with("done\t"),
            "Expected done status, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_env_evergreen_status() {
        let input = r#"{"cookbook":"git","name":"proj","status":{"type":"evergreen"},"scores":{"launcher":0.5,"slot":0.1}}"#;
        let entries = format_entries(input);
        assert!(
            entries[0].starts_with("evergreen\t"),
            "Expected evergreen status, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_env_uncooked_status() {
        let input = r#"{"cookbook":"git","name":"proj","status":{"type":"uncooked"},"scores":{"launcher":0.5,"slot":0.1}}"#;
        let entries = format_entries(input);
        assert!(
            entries[0].starts_with("*\t"),
            "Uncooked env should have * status, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_recipe_has_empty_status() {
        let input = r#"{"cookbook":"git","name":"my-recipe"}"#;
        let entries = format_entries(input);
        assert!(
            entries[0].starts_with("\tgit\t"),
            "Recipe should have empty status column, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_done_envs_after_recipes() {
        let input = [
            r#"{"cookbook":"","name":"done-env","status":{"type":"done"},"scores":{"launcher":0.9,"slot":0.1}}"#,
            r#"{"cookbook":"","name":"active-env","status":{"type":"cooked","phase":"active"},"scores":{"launcher":0.5,"slot":0.1}}"#,
            r#"{"cookbook":"git","name":"recipe-a"}"#,
        ]
        .join("\n");
        let entries = format_entries(&input);
        assert_eq!(entries.len(), 3);
        assert!(
            entries[0].contains("\tactive-env\t"),
            "Active env should be first, got: {}",
            entries[0]
        );
        assert!(
            entries[1].contains("\trecipe-a\t"),
            "Recipe should be second, got: {}",
            entries[1]
        );
        assert!(
            entries[2].contains("\tdone-env\t"),
            "Done env should be last, got: {}",
            entries[2]
        );
    }
}

fn main() -> anyhow::Result<()> {
    let _guard = enwiro_sdk::init_logging("enwiro-bridge-rofi.log");

    let rofi_retv = env::var("ROFI_RETV").unwrap_or_else(|_| "0".to_string());
    let args: Vec<String> = env::args().collect();

    tracing::debug!(rofi_retv = %rofi_retv, "Bridge invoked");

    match rofi_retv.as_str() {
        "0" => list_entries()?,
        "1" | "2" => {
            if let Some(selection) = args.get(1).map(|s| s.trim())
                && !selection.is_empty()
            {
                activate_selection(selection)?;
            }
        }
        _ => list_entries()?,
    }

    Ok(())
}
