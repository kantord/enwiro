use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::Context;

use crate::client::{CachedRecipe, CookbookClient, CookbookTrait};
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

const USER_IDLE_THRESHOLD: Duration = Duration::from_secs(5400); // 90 minutes

/// Returns true if the user has been idle longer than the threshold.
/// When idle time is unavailable (e.g. Wayland without support), returns false
/// so the daemon keeps running rather than dying unexpectedly.
fn check_idle_with_timeout(get_idle: impl Fn() -> Option<Duration>, threshold: Duration) -> bool {
    match get_idle() {
        Some(idle) => idle > threshold,
        None => false,
    }
}

/// Returns true if the user has been idle longer than 90 minutes.
pub fn check_idle() -> bool {
    check_idle_with_timeout(
        || system_idle_time::get_idle_time().ok(),
        USER_IDLE_THRESHOLD,
    )
}

/// Parse the PID file, returning (pid, optional binary mtime).
/// Format: first line is the PID, optional second line is binary mtime as Unix seconds.
fn read_pid_file(runtime_dir: &Path) -> Option<(i32, Option<SystemTime>)> {
    let content = fs::read_to_string(runtime_dir.join("daemon.pid")).ok()?;
    let mut lines = content.lines();
    let pid: i32 = lines.next()?.trim().parse().ok()?;
    let mtime = lines
        .next()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
    Some((pid, mtime))
}

/// Write the current process PID and binary mtime to the PID file.
pub fn write_pid_file(runtime_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(runtime_dir).context("Could not create runtime directory")?;
    let pid_path = runtime_dir.join("daemon.pid");
    let exe_mtime_secs = get_current_exe_mtime()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let content = match exe_mtime_secs {
        Some(secs) => format!("{}\n{}", std::process::id(), secs),
        None => std::process::id().to_string(),
    };
    fs::write(&pid_path, content).context("Could not write PID file")?;
    Ok(())
}

/// Remove the PID file on daemon exit, but only if it still belongs to this process.
pub fn remove_pid_file(runtime_dir: &Path) {
    let pid_path = runtime_dir.join("daemon.pid");
    if let Some((stored_pid, _)) = read_pid_file(runtime_dir)
        && stored_pid == std::process::id() as i32
    {
        let _ = fs::remove_file(&pid_path);
    }
}

/// Check if a daemon is currently running by reading the PID file and
/// sending signal 0 (no-op) to the process.
pub fn is_daemon_running(runtime_dir: &Path) -> bool {
    match read_pid_file(runtime_dir) {
        Some((pid, _)) => unsafe { libc::kill(pid, 0) == 0 },
        None => false,
    }
}

fn get_current_exe_mtime() -> Option<SystemTime> {
    std::env::current_exe()
        .ok()
        .and_then(|p| fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
}

/// Returns true if the running daemon's binary is older than the current executable.
fn needs_restart_with_mtime(
    runtime_dir: &Path,
    get_exe_mtime: impl Fn() -> Option<SystemTime>,
) -> bool {
    let stored_mtime = match read_pid_file(runtime_dir) {
        Some((_, Some(t))) => t,
        _ => return false,
    };
    match get_exe_mtime() {
        Some(current) => current > stored_mtime,
        None => false,
    }
}

/// Spawn the daemon as a detached background process.
/// Returns Ok(true) if a new daemon was spawned, Ok(false) if one was already running.
pub fn ensure_daemon_running(runtime_dir: &Path) -> anyhow::Result<bool> {
    if is_daemon_running(runtime_dir) {
        if needs_restart_with_mtime(runtime_dir, get_current_exe_mtime) {
            tracing::info!("Binary updated, restarting daemon");
            if let Some((pid, _)) = read_pid_file(runtime_dir) {
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
        } else {
            return Ok(false);
        }
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

/// A cached recipe with its cookbook's priority, used for global sorting.
struct SortableCachedRecipe {
    cached: CachedRecipe,
    priority: u32,
}

/// Collect recipes from all cookbooks as JSON lines, sorted globally
/// by (sort_order, cookbook priority, name).
/// Errors in individual cookbooks are logged and skipped.
pub fn collect_all_recipes(cookbooks: &[Box<dyn CookbookTrait>]) -> String {
    let mut all_recipes: Vec<SortableCachedRecipe> = Vec::new();

    for cookbook in cookbooks {
        match cookbook.list_recipes() {
            Ok(recipes) => {
                for recipe in recipes {
                    all_recipes.push(SortableCachedRecipe {
                        cached: CachedRecipe {
                            cookbook: cookbook.name().to_string(),
                            name: recipe.name,
                            description: recipe.description,
                            sort_order: recipe.sort_order,
                        },
                        priority: cookbook.priority(),
                    });
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

    all_recipes.sort_by(|a, b| {
        a.cached
            .sort_order
            .cmp(&b.cached.sort_order)
            .then_with(|| a.priority.cmp(&b.priority))
            .then_with(|| a.cached.name.cmp(&b.cached.name))
    });

    let mut output = String::new();
    for item in all_recipes {
        if let Ok(json) = serde_json::to_string(&item.cached) {
            output.push_str(&json);
            output.push('\n');
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

        if check_idle() {
            tracing::info!("User idle threshold reached, exiting");
            remove_pid_file(&dir);
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::CachedRecipe;
    use crate::test_utils::test_utilities::{FailingCookbook, FakeCookbook};

    fn parse_cached_lines(output: &str) -> Vec<CachedRecipe> {
        output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn test_collect_all_recipes_includes_description() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> =
            vec![Box::new(FakeCookbook::new_with_descriptions(
                "github",
                vec![("owner/repo#42", Some("Fix auth bug"))],
                vec![],
            ))];
        let output = collect_all_recipes(&cookbooks);
        let entries = parse_cached_lines(&output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].cookbook, "github");
        assert_eq!(entries[0].name, "owner/repo#42");
        assert_eq!(entries[0].description.as_deref(), Some("Fix auth bug"));
    }

    #[test]
    fn test_collect_all_recipes_omits_description_when_none() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![Box::new(
            FakeCookbook::new_with_descriptions("git", vec![("repo-a", None)], vec![]),
        )];
        let output = collect_all_recipes(&cookbooks);
        let entries = parse_cached_lines(&output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "repo-a");
        assert!(entries[0].description.is_none());
    }

    #[test]
    fn test_collect_all_recipes_formats_output() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["repo-a", "repo-b"],
            vec![],
        ))];
        let output = collect_all_recipes(&cookbooks);
        let entries = parse_cached_lines(&output);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "repo-a");
        assert_eq!(entries[1].name, "repo-b");
    }

    #[test]
    fn test_collect_all_recipes_multiple_cookbooks() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![
            Box::new(FakeCookbook::new("git", vec!["repo-a"], vec![])),
            Box::new(FakeCookbook::new("npm", vec!["pkg-x"], vec![])),
        ];
        let output = collect_all_recipes(&cookbooks);
        let entries = parse_cached_lines(&output);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"repo-a"));
        assert!(names.contains(&"pkg-x"));
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

    fn write_test_pid_file(dir: &Path, pid: i32, mtime_secs: Option<u64>) {
        let content = match mtime_secs {
            Some(secs) => format!("{}\n{}", pid, secs),
            None => pid.to_string(),
        };
        std::fs::write(dir.join("daemon.pid"), content).unwrap();
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
    fn test_write_pid_file_includes_mtime() {
        let dir = tempfile::tempdir().unwrap();
        write_pid_file(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join("daemon.pid")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "PID file should have PID and mtime lines");
        assert!(
            lines[0].trim().parse::<u32>().is_ok(),
            "First line should be a PID"
        );
        assert!(
            lines[1].trim().parse::<u64>().is_ok(),
            "Second line should be a mtime timestamp"
        );
    }

    #[test]
    fn test_remove_pid_file_does_not_remove_other_pid() {
        let dir = tempfile::tempdir().unwrap();
        write_test_pid_file(dir.path(), 999999, None);
        remove_pid_file(dir.path());
        assert!(
            dir.path().join("daemon.pid").exists(),
            "Should not remove another process's PID file"
        );
    }

    #[test]
    fn test_needs_restart_when_binary_is_newer() {
        let dir = tempfile::tempdir().unwrap();
        write_test_pid_file(dir.path(), 999999, Some(1000));
        let new_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(2000);
        assert!(needs_restart_with_mtime(dir.path(), || Some(new_mtime)));
    }

    #[test]
    fn test_no_restart_when_binary_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        write_test_pid_file(dir.path(), 999999, Some(1000));
        let same_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        assert!(!needs_restart_with_mtime(dir.path(), || Some(same_mtime)));
    }

    #[test]
    fn test_no_restart_when_no_mtime_stored() {
        let dir = tempfile::tempdir().unwrap();
        write_test_pid_file(dir.path(), 999999, None);
        let new_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(2000);
        assert!(!needs_restart_with_mtime(dir.path(), || Some(new_mtime)));
    }

    #[test]
    fn test_no_restart_when_exe_mtime_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        write_test_pid_file(dir.path(), 999999, Some(1000));
        assert!(!needs_restart_with_mtime(dir.path(), || None));
    }

    #[test]
    fn test_user_is_idle_when_idle_time_exceeds_threshold() {
        let threshold = Duration::from_secs(5400);
        let idle_time = Duration::from_secs(6000);
        assert!(check_idle_with_timeout(|| Some(idle_time), threshold));
    }

    #[test]
    fn test_user_is_not_idle_when_idle_time_below_threshold() {
        let threshold = Duration::from_secs(5400);
        let idle_time = Duration::from_secs(60);
        assert!(!check_idle_with_timeout(|| Some(idle_time), threshold));
    }

    #[test]
    fn test_user_is_not_idle_when_idle_time_equals_threshold() {
        let threshold = Duration::from_secs(5400);
        assert!(!check_idle_with_timeout(|| Some(threshold), threshold));
    }

    #[test]
    fn test_user_is_not_idle_when_idle_time_unavailable() {
        let threshold = Duration::from_secs(5400);
        assert!(!check_idle_with_timeout(|| None, threshold));
    }

    #[test]
    fn test_write_and_read_cache() {
        let dir = tempfile::tempdir().unwrap();
        let content = r#"{"cookbook":"git","name":"my-repo"}
{"cookbook":"chezmoi","name":"chezmoi"}
"#;
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
        write_cache_atomic(dir.path(), r#"{"cookbook":"git","name":"old-repo"}"#).unwrap();
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
        let content = "{\"cookbook\":\"git\",\"name\":\"fresh-repo\"}\n";
        write_cache_atomic(dir.path(), content).unwrap();
        let read = read_cached_recipes(dir.path()).unwrap();
        assert_eq!(read, Some(content.to_string()));
    }

    #[test]
    fn test_collect_all_recipes_sorts_by_priority() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![
            Box::new(FakeCookbook::new("github", vec!["repo#42"], vec![]).with_priority(30)),
            Box::new(FakeCookbook::new("git", vec!["my-repo"], vec![]).with_priority(10)),
        ];
        let output = collect_all_recipes(&cookbooks);
        let entries = parse_cached_lines(&output);
        assert_eq!(
            entries[0].cookbook, "git",
            "Higher priority (lower number) should come first"
        );
        assert_eq!(entries[0].name, "my-repo");
        assert_eq!(entries[1].cookbook, "github");
        assert_eq!(entries[1].name, "repo#42");
    }

    #[test]
    fn test_collect_all_recipes_sorts_by_name_on_priority_tie() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![
            Box::new(FakeCookbook::new("npm", vec!["pkg-x"], vec![]).with_priority(20)),
            Box::new(FakeCookbook::new("git", vec!["repo-a"], vec![]).with_priority(20)),
        ];
        let output = collect_all_recipes(&cookbooks);
        let entries = parse_cached_lines(&output);
        // Global sort: (sort_order=0, priority=20, name) — alphabetical by recipe name
        assert_eq!(
            entries[0].name, "pkg-x",
            "Same sort_order and priority should tie-break by recipe name"
        );
        assert_eq!(entries[0].cookbook, "npm");
        assert_eq!(entries[1].name, "repo-a");
        assert_eq!(entries[1].cookbook, "git");
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
        let entries = parse_cached_lines(&output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].cookbook, "git");
        assert_eq!(entries[0].name, "repo-a");
    }

    #[test]
    fn test_collect_all_recipes_sorts_globally_by_sort_order() {
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![
            Box::new(
                FakeCookbook::new("git", vec!["git-repo-a", "git-repo-b"], vec![])
                    .with_priority(10)
                    .with_sort_orders(vec![0, 50]),
            ),
            Box::new(
                FakeCookbook::new("github", vec!["gh-issue-1", "gh-issue-2"], vec![])
                    .with_priority(30)
                    .with_sort_orders(vec![0, 50]),
            ),
        ];
        let output = collect_all_recipes(&cookbooks);
        let entries = parse_cached_lines(&output);
        assert_eq!(entries.len(), 4);
        // sort_order=0 items first (git before github due to priority tiebreak)
        assert_eq!(entries[0].name, "git-repo-a");
        assert_eq!(entries[0].sort_order, 0);
        assert_eq!(entries[1].name, "gh-issue-1");
        assert_eq!(entries[1].sort_order, 0);
        // sort_order=50 items next
        assert_eq!(entries[2].name, "git-repo-b");
        assert_eq!(entries[2].sort_order, 50);
        assert_eq!(entries[3].name, "gh-issue-2");
        assert_eq!(entries[3].sort_order, 50);
    }

    #[test]
    fn test_collect_all_recipes_interleaves_cookbooks_by_sort_order() {
        // GitHub recipe with sort_order=0 should appear before git recipe with sort_order=50,
        // even though git has higher priority
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![
            Box::new(
                FakeCookbook::new("git", vec!["low-priority-branch"], vec![])
                    .with_priority(10)
                    .with_sort_orders(vec![80]),
            ),
            Box::new(
                FakeCookbook::new("github", vec!["hot-issue"], vec![])
                    .with_priority(30)
                    .with_sort_orders(vec![0]),
            ),
        ];
        let output = collect_all_recipes(&cookbooks);
        let entries = parse_cached_lines(&output);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].name, "hot-issue",
            "Lower sort_order should come first regardless of cookbook priority"
        );
        assert_eq!(entries[1].name, "low-priority-branch");
    }
}
