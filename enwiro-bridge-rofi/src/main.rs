use anyhow::Context;
use std::collections::HashSet;
use std::env;
use std::process::Command;

fn list_entries() -> anyhow::Result<()> {
    let output = Command::new("enwiro")
        .arg("list-all")
        .output()
        .context("Failed to run enwiro list-all")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
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
    // We intentionally spawn without calling .wait(). This lets the bridge
    // exit immediately so rofi can close, while enwiro activate continues
    // in the background (e.g. cooking an environment from a git recipe may
    // take a while). The child becomes a short-lived zombie until this
    // process exits, at which point init reaps it.
    Command::new("enwiro")
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
    let rofi_retv = env::var("ROFI_RETV").unwrap_or_else(|_| "0".to_string());
    let args: Vec<String> = env::args().collect();

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
