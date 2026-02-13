use anyhow::Context;
use std::collections::HashSet;
use std::env;
use std::path::PathBuf;
use std::process::Command;
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

    // Deduplicate by name — an environment may appear both as an existing
    // environment ("_: name") and as a recipe ("cookbook: name").
    let mut seen = HashSet::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((source, name)) = line.split_once(": ")
            && seen.insert(name.to_string())
        {
            println!("{}\0info\x1f{}", name, source);
        }
    }

    Ok(())
}

fn activate_selection(selection: &str) -> anyhow::Result<()> {
    tracing::debug!(selection = %selection, "Activating selection");
    // We intentionally spawn without calling .wait(). This lets the bridge
    // exit immediately so rofi can close, while enwiro activate continues
    // in the background (e.g. cooking an environment from a git recipe may
    // take a while). The child becomes a short-lived zombie until this
    // process exits, at which point init reaps it.
    Command::new(enwiro_bin()?)
        .arg("activate")
        .arg(selection)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .context("Failed to spawn enwiro activate")?;

    Ok(())
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
