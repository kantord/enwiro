use anyhow::Context;
use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::path::PathBuf;
use std::process::Command;

#[derive(Deserialize)]
struct CacheEntry {
    cookbook: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
}
fn enwiro_bin() -> anyhow::Result<PathBuf> {
    if let Ok(path) = env::var("ENWIRO_BIN") {
        tracing::debug!(path = %path, "Using ENWIRO_BIN env var");
        return Ok(PathBuf::from(path));
    }
    let exe = env::current_exe().context("could not determine own executable path")?;
    let dir = exe.parent().context("executable has no parent directory")?;
    let bin = dir.join("enwiro");
    tracing::debug!(path = %bin.display(), "Resolved enwiro binary from exe parent");
    Ok(bin)
}

/// Format raw `enwiro list-all` JSON lines output into rofi script-mode entries.
/// Deduplicates by name and formats as tab-separated columns with rofi metadata.
fn format_entries(input: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<CacheEntry>(line) {
            let description = entry.description.as_deref().unwrap_or("");
            if seen.insert(entry.name.clone()) {
                entries.push(format!(
                    "{}\t{}\t{}\0info\x1f{}",
                    entry.cookbook, entry.name, description, entry.cookbook
                ));
            }
        }
    }
    entries
}

fn list_entries() -> anyhow::Result<()> {
    tracing::debug!("Listing entries via enwiro list-all");
    let output = Command::new(enwiro_bin()?)
        .arg("list-all")
        .output()
        .context("Failed to run enwiro list-all")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(%stderr, "enwiro list-all failed");
        anyhow::bail!("enwiro list-all failed: {}", stderr);
    }

    let stdout = String::from_utf8(output.stdout)?;
    for entry in format_entries(&stdout) {
        println!("{}", entry);
    }

    Ok(())
}

/// Strip the source and description columns from a rofi selection.
/// Rofi passes back "source\tname\tdescription" but enwiro expects just "name".
fn extract_recipe_name(selection: &str) -> &str {
    let after_source = selection
        .split_once('\t')
        .map_or(selection, |(_, rest)| rest);
    after_source
        .split_once('\t')
        .map_or(after_source, |(name, _)| name)
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
            entries[0].starts_with("git\tmy-project\t"),
            "Expected tab-separated columns, got: {}",
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
            entries[0].starts_with("_\tmy-project"),
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
    fn test_extract_recipe_name_strips_source_column() {
        assert_eq!(extract_recipe_name("git\tmy-project\t"), "my-project");
    }

    #[test]
    fn test_extract_recipe_name_strips_description_column() {
        assert_eq!(
            extract_recipe_name("github\towner/repo#42\tFix auth bug"),
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
        assert!(entries[0].starts_with("git\tproject-a\t"));
        assert!(entries[1].starts_with("chezmoi\tchezmoi\t"));
        assert!(entries[2].starts_with("git\tproject-b\t"));
    }

    #[test]
    fn test_format_entries_with_description() {
        let input = r#"{"cookbook":"github","name":"owner/repo#42","description":"Fix auth bug"}"#;
        let entries = format_entries(input);
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].starts_with("github\towner/repo#42\tFix auth bug"),
            "Expected description in third column, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_format_entries_without_description_has_empty_column() {
        let input = r#"{"cookbook":"git","name":"my-project"}"#;
        let entries = format_entries(input);
        assert!(
            entries[0].starts_with("git\tmy-project\t\0"),
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
}

fn main() -> anyhow::Result<()> {
    let _guard = enwiro_logging::init_logging("enwiro-bridge-rofi.log");

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
