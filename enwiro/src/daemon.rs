use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::Context;

use crate::client::{CookbookClient, CookbookTrait};
use crate::plugin::{PluginKind, get_plugins};

/// Returns the directory for daemon runtime files (PID, cache, heartbeat).
/// Prefers $XDG_RUNTIME_DIR/enwiro, falls back to $XDG_CACHE_HOME/enwiro/run.
pub fn runtime_dir() -> anyhow::Result<PathBuf> {
    let base = dirs::runtime_dir()
        .or_else(|| dirs::cache_dir().map(|d| d.join("run")))
        .context("Could not determine runtime or cache directory")?;
    Ok(base.join("enwiro"))
}

/// Atomically write content to the cache file.
/// Writes to a temporary file in the same directory, then renames.
pub fn write_cache_atomic(runtime_dir: &Path, content: &str) -> anyhow::Result<()> {
    fs::create_dir_all(runtime_dir).context("Could not create runtime directory")?;
    let cache_path = runtime_dir.join("recipes.cache");
    let tmp_path = runtime_dir.join("recipes.cache.tmp");
    fs::write(&tmp_path, content).context("Could not write temporary cache file")?;
    fs::rename(&tmp_path, &cache_path).context("Could not rename cache file into place")?;
    tracing::debug!(path = %cache_path.display(), "Cache file updated");
    Ok(())
}

/// Maximum age for a cache file to be considered valid (refresh interval + 30s buffer).
const CACHE_MAX_AGE: Duration = Duration::from_secs(70); // 40s + 30s

/// Read the cached recipes. Returns None if cache doesn't exist or is stale.
pub fn read_cached_recipes(runtime_dir: &Path) -> anyhow::Result<Option<String>> {
    let cache_path = runtime_dir.join("recipes.cache");
    let metadata = match fs::metadata(&cache_path) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    if let Ok(modified) = metadata.modified() {
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::ZERO);
        if age > CACHE_MAX_AGE {
            tracing::debug!(age_secs = age.as_secs(), "Cache is stale, ignoring");
            return Ok(None);
        }
    }
    let content = fs::read_to_string(&cache_path).context("Could not read cache file")?;
    Ok(Some(content))
}

const IDLE_TIMEOUT: Duration = Duration::from_secs(10800); // 3 hours

/// Touch the heartbeat file to indicate the daemon's output is being consumed.
pub fn touch_heartbeat(runtime_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(runtime_dir).context("Could not create runtime directory")?;
    let heartbeat_path = runtime_dir.join("heartbeat");
    fs::write(&heartbeat_path, "").context("Could not touch heartbeat file")?;
    Ok(())
}

/// Check if the daemon has been idle for longer than the given timeout.
/// Returns true if the daemon should exit due to inactivity.
fn check_idle_with_timeout(runtime_dir: &Path, timeout: Duration) -> bool {
    let heartbeat_path = runtime_dir.join("heartbeat");
    match fs::metadata(&heartbeat_path) {
        Ok(metadata) => match metadata.modified() {
            Ok(modified) => {
                let elapsed = SystemTime::now()
                    .duration_since(modified)
                    .unwrap_or(Duration::ZERO);
                elapsed > timeout
            }
            Err(_) => false,
        },
        Err(_) => false,
    }
}

/// Check if the daemon has been idle (no heartbeat touch) for longer than 3 hours.
pub fn check_idle(runtime_dir: &Path) -> bool {
    check_idle_with_timeout(runtime_dir, IDLE_TIMEOUT)
}

/// Write the current process PID to the PID file.
pub fn write_pid_file(runtime_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(runtime_dir).context("Could not create runtime directory")?;
    let pid_path = runtime_dir.join("daemon.pid");
    fs::write(&pid_path, std::process::id().to_string()).context("Could not write PID file")?;
    Ok(())
}

/// Remove the PID file on daemon exit.
pub fn remove_pid_file(runtime_dir: &Path) {
    let pid_path = runtime_dir.join("daemon.pid");
    let _ = fs::remove_file(&pid_path);
}

/// Check if a daemon is currently running by reading the PID file and
/// sending signal 0 (no-op) to the process.
pub fn is_daemon_running(runtime_dir: &Path) -> bool {
    let pid_path = runtime_dir.join("daemon.pid");
    let pid_str = match fs::read_to_string(&pid_path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let pid: i32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => return false,
    };
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Spawn the daemon as a detached background process.
/// Returns Ok(true) if a new daemon was spawned, Ok(false) if one was already running.
pub fn ensure_daemon_running(runtime_dir: &Path) -> anyhow::Result<bool> {
    if is_daemon_running(runtime_dir) {
        return Ok(false);
    }

    tracing::info!("Spawning background daemon");
    std::process::Command::new(std::env::current_exe()?)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Could not spawn daemon process")?;

    Ok(true)
}

/// Collect recipe lines from all cookbooks, formatted as "cookbook_name: recipe_name\n".
/// Errors in individual cookbooks are logged and skipped.
pub fn collect_all_recipes(cookbooks: &[Box<dyn CookbookTrait>]) -> String {
    let mut sorted: Vec<_> = cookbooks.iter().collect();
    sorted.sort_by(|a, b| {
        a.priority()
            .cmp(&b.priority())
            .then_with(|| a.name().cmp(b.name()))
    });
    let mut output = String::new();
    for cookbook in sorted {
        match cookbook.list_recipes() {
            Ok(recipes) => {
                for recipe in recipes {
                    match &recipe.description {
                        Some(desc) => output.push_str(&format!(
                            "{}: {}\t{}\n",
                            cookbook.name(),
                            recipe.name,
                            desc
                        )),
                        None => output.push_str(&format!("{}: {}\n", cookbook.name(), recipe.name)),
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    cookbook = %cookbook.name(),
                    error = %e,
                    "Skipping cookbook due to error"
                );
            }
        }
    }
    output
}

const REFRESH_INTERVAL: Duration = Duration::from_secs(40);

/// Entry point for the daemon. Called when `enwiro daemon` is invoked.
pub fn run_daemon() -> anyhow::Result<()> {
    // Detach from session
    let setsid_result = unsafe { libc::setsid() };
    if setsid_result == -1 {
        tracing::warn!("setsid() failed, continuing anyway");
    }

    let dir = runtime_dir()?;
    fs::create_dir_all(&dir)?;

    // Write PID file
    write_pid_file(&dir)?;

    // Register signal handler
    let term = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&term))?;
    signal_hook::flag::register(signal_hook::consts::SIGHUP, Arc::clone(&term))?;

    // Initial heartbeat (so we don't immediately exit)
    touch_heartbeat(&dir)?;

    tracing::info!(pid = std::process::id(), "Daemon started");

    loop {
        // Discover plugins fresh each cycle (new cookbooks may be installed)
        let plugins = get_plugins(PluginKind::Cookbook);
        let cookbooks: Vec<Box<dyn CookbookTrait>> = plugins
            .into_iter()
            .map(|p| Box::new(CookbookClient::new(p)) as Box<dyn CookbookTrait>)
            .collect();

        let recipes = collect_all_recipes(&cookbooks);
        if let Err(e) = write_cache_atomic(&dir, &recipes) {
            tracing::error!(error = %e, "Failed to write cache");
        }

        // Sleep in 1-second increments, checking for termination signal
        let mut elapsed = Duration::ZERO;
        while elapsed < REFRESH_INTERVAL {
            if term.load(Ordering::Relaxed) {
                tracing::info!("Received termination signal, exiting");
                remove_pid_file(&dir);
                return Ok(());
            }
            std::thread::sleep(Duration::from_secs(1));
            elapsed += Duration::from_secs(1);
        }

        // Check idle timeout
        if check_idle(&dir) {
            tracing::info!("Idle timeout reached, exiting");
            remove_pid_file(&dir);
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_utilities::{FailingCookbook, FakeCookbook};

    #[test]
    fn test_collect_all_recipes_includes_description() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> =
            vec![Box::new(FakeCookbook::new_with_descriptions(
                "github",
                vec![("owner/repo#42", Some("Fix auth bug"))],
                vec![],
            ))];
        let output = collect_all_recipes(&cookbooks);
        assert_eq!(output, "github: owner/repo#42\tFix auth bug\n");
    }

    #[test]
    fn test_collect_all_recipes_omits_tab_when_no_description() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![Box::new(
            FakeCookbook::new_with_descriptions("git", vec![("repo-a", None)], vec![]),
        )];
        let output = collect_all_recipes(&cookbooks);
        assert_eq!(output, "git: repo-a\n");
        assert!(!output.contains('\t'));
    }

    #[test]
    fn test_collect_all_recipes_formats_output() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["repo-a", "repo-b"],
            vec![],
        ))];
        let output = collect_all_recipes(&cookbooks);
        assert_eq!(output, "git: repo-a\ngit: repo-b\n");
    }

    #[test]
    fn test_collect_all_recipes_multiple_cookbooks() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![
            Box::new(FakeCookbook::new("git", vec!["repo-a"], vec![])),
            Box::new(FakeCookbook::new("npm", vec!["pkg-x"], vec![])),
        ];
        let output = collect_all_recipes(&cookbooks);
        assert!(output.contains("git: repo-a\n"));
        assert!(output.contains("npm: pkg-x\n"));
    }

    #[test]
    fn test_collect_all_recipes_empty() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![];
        let output = collect_all_recipes(&cookbooks);
        assert_eq!(output, "");
    }

    #[test]
    fn test_is_daemon_running_no_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_daemon_running(dir.path()));
    }

    #[test]
    fn test_is_daemon_running_with_own_pid() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("daemon.pid"),
            std::process::id().to_string(),
        )
        .unwrap();
        assert!(is_daemon_running(dir.path()));
    }

    #[test]
    fn test_is_daemon_running_stale_pid() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("daemon.pid"), "999999999").unwrap();
        assert!(!is_daemon_running(dir.path()));
    }

    #[test]
    fn test_is_daemon_running_invalid_pid_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("daemon.pid"), "not-a-number").unwrap();
        assert!(!is_daemon_running(dir.path()));
    }

    #[test]
    fn test_write_and_remove_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        write_pid_file(dir.path()).unwrap();
        assert!(dir.path().join("daemon.pid").exists());
        remove_pid_file(dir.path());
        assert!(!dir.path().join("daemon.pid").exists());
    }

    #[test]
    fn test_fresh_heartbeat_is_not_idle() {
        let dir = tempfile::tempdir().unwrap();
        touch_heartbeat(dir.path()).unwrap();
        assert!(!check_idle(dir.path()));
    }

    #[test]
    fn test_no_heartbeat_file_is_not_idle() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!check_idle(dir.path()));
    }

    #[test]
    fn test_old_heartbeat_is_idle() {
        let dir = tempfile::tempdir().unwrap();
        touch_heartbeat(dir.path()).unwrap();
        let past = filetime::FileTime::from_system_time(
            std::time::SystemTime::now() - std::time::Duration::from_secs(14400),
        );
        filetime::set_file_mtime(dir.path().join("heartbeat"), past).unwrap();
        assert!(check_idle_with_timeout(
            dir.path(),
            std::time::Duration::from_secs(10800)
        ));
    }

    #[test]
    fn test_write_and_read_cache() {
        let dir = tempfile::tempdir().unwrap();
        let content = "git: my-repo\nchezmoi: chezmoi\n";
        write_cache_atomic(dir.path(), content).unwrap();
        let read = read_cached_recipes(dir.path()).unwrap();
        assert_eq!(read, Some(content.to_string()));
    }

    #[test]
    fn test_read_cache_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let read = read_cached_recipes(dir.path()).unwrap();
        assert_eq!(read, None);
    }

    #[test]
    fn test_write_cache_creates_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested").join("enwiro");
        write_cache_atomic(&nested, "test").unwrap();
        let read = read_cached_recipes(&nested).unwrap();
        assert_eq!(read, Some("test".to_string()));
    }

    #[test]
    fn test_read_cache_returns_none_when_stale() {
        let dir = tempfile::tempdir().unwrap();
        write_cache_atomic(dir.path(), "git: old-repo\n").unwrap();
        // Backdate cache to 10 minutes ago (older than 40s + 30s staleness threshold)
        let past = filetime::FileTime::from_system_time(
            std::time::SystemTime::now() - std::time::Duration::from_secs(600),
        );
        filetime::set_file_mtime(dir.path().join("recipes.cache"), past).unwrap();
        let read = read_cached_recipes(dir.path()).unwrap();
        assert_eq!(
            read, None,
            "Stale cache (older than refresh interval + 30s) should be treated as missing"
        );
    }

    #[test]
    fn test_read_cache_returns_content_when_fresh() {
        let dir = tempfile::tempdir().unwrap();
        write_cache_atomic(dir.path(), "git: fresh-repo\n").unwrap();
        // Cache was just written — should be fresh
        let read = read_cached_recipes(dir.path()).unwrap();
        assert_eq!(read, Some("git: fresh-repo\n".to_string()));
    }

    #[test]
    fn test_collect_all_recipes_sorts_by_priority() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![
            Box::new(FakeCookbook::new("github", vec!["repo#42"], vec![]).with_priority(30)),
            Box::new(FakeCookbook::new("git", vec!["my-repo"], vec![]).with_priority(10)),
        ];
        let output = collect_all_recipes(&cookbooks);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(
            lines[0], "git: my-repo",
            "Higher priority (lower number) should come first"
        );
        assert_eq!(lines[1], "github: repo#42");
    }

    #[test]
    fn test_collect_all_recipes_sorts_by_name_on_priority_tie() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![
            Box::new(FakeCookbook::new("npm", vec!["pkg-x"], vec![]).with_priority(20)),
            Box::new(FakeCookbook::new("git", vec!["repo-a"], vec![]).with_priority(20)),
        ];
        let output = collect_all_recipes(&cookbooks);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(
            lines[0], "git: repo-a",
            "Same priority should tie-break alphabetically"
        );
        assert_eq!(lines[1], "npm: pkg-x");
    }

    #[test]
    fn test_collect_all_recipes_skips_failing_cookbook() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![
            Box::new(FailingCookbook {
                cookbook_name: "broken".into(),
            }),
            Box::new(FakeCookbook::new("git", vec!["repo-a"], vec![])),
        ];
        let output = collect_all_recipes(&cookbooks);
        assert_eq!(output, "git: repo-a\n");
    }
}
