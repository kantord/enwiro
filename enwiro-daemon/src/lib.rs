//! Background daemon for enwiro.
//!
//! Refreshes the cookbook recipe cache on a periodic interval (when the user
//! is active) and forwards workspace switch events from the adapter's
//! `listen` subcommand. The caller provides a `workspaces_directory` and a
//! `on_workspace_switch` callback; everything else (PID file, signal
//! handling, idle detection, cache file location) is owned by this crate.

pub mod config;
pub mod meta;
pub mod rpc;
pub use config::ConfigurationValues;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::Context;

use std::collections::HashMap;

use enwiro_sdk::client::{CachedRecipe, CookbookClient, CookbookTrait};
use enwiro_sdk::cookbook::{CookbookPayload, Recipe};
use enwiro_sdk::listen::RecipeUpdate;
use enwiro_sdk::plugin::{PluginKind, get_plugins};
use optative_process_pool::{ProcessIdentity, ProcessPool, ProcessSource, StreamItem, StreamKind};

/// Per-cookbook state assembled from the listen-stdout stream. The
/// cache content is rebuilt from this map on every change.
struct CookbookEntry {
    priority: u32,
    recipes: Vec<Recipe>,
}

/// Returns the directory for daemon runtime files (PID, cache, heartbeat).
/// Prefers $XDG_RUNTIME_DIR/enwiro, falls back to $XDG_CACHE_HOME/enwiro/run.
fn runtime_dir() -> anyhow::Result<PathBuf> {
    let base = dirs::runtime_dir()
        .or_else(|| dirs::cache_dir().map(|d| d.join("run")))
        .context("Could not determine runtime or cache directory")?;
    Ok(base.join("enwiro"))
}

/// Read-only view of the daemon's on-disk cache. Resolves the runtime
/// directory the same way the daemon does, so callers don't need to know
/// where the cache file lives.
pub struct DaemonCache {
    runtime_dir: PathBuf,
}

impl DaemonCache {
    /// Resolve the runtime directory and return a handle. Does not require
    /// the daemon to be running.
    pub fn open() -> anyhow::Result<Self> {
        Ok(Self {
            runtime_dir: runtime_dir()?,
        })
    }

    /// Open a handle backed by an explicit runtime directory instead of the
    /// XDG-derived one. Useful for tests and for tools pointing at a specific
    /// daemon's cache.
    pub fn with_runtime_dir(runtime_dir: PathBuf) -> Self {
        Self { runtime_dir }
    }

    /// Read the cached recipes JSONL file. Returns `Ok(None)` when the cache
    /// is missing or stale.
    pub fn read_recipes(&self) -> anyhow::Result<Option<String>> {
        read_cached_recipes(&self.runtime_dir)
    }

    /// The runtime directory in use. Exposed primarily so callers can mention
    /// it in error messages.
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }
}

/// Atomically write content to the cache file.
pub(crate) fn write_cache_atomic(runtime_dir: &Path, content: &str) -> anyhow::Result<()> {
    let cache_path = runtime_dir.join("recipes.cache");
    enwiro_sdk::fs::atomic_write(&cache_path, content.as_bytes())
        .with_context(|| format!("Could not write cache file {}", cache_path.display()))?;
    tracing::debug!(path = %cache_path.display(), "Cache file updated");
    Ok(())
}

/// Read the cached recipes. Returns None if the cache file doesn't exist.
fn read_cached_recipes(runtime_dir: &Path) -> anyhow::Result<Option<String>> {
    let cache_path = runtime_dir.join("recipes.cache");
    if !cache_path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&cache_path).context("Could not read cache file")?;
    Ok(Some(content))
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

/// Write the current process PID to the PID file.
pub(crate) fn write_pid_file(runtime_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(runtime_dir).context("Could not create runtime directory")?;
    let pid_path = runtime_dir.join("daemon.pid");
    fs::write(&pid_path, std::process::id().to_string()).context("Could not write PID file")?;
    Ok(())
}

/// Remove the PID file on daemon exit, but only if it still belongs to this process.
pub(crate) fn remove_pid_file(runtime_dir: &Path) {
    let pid_path = runtime_dir.join("daemon.pid");
    if let Some((stored_pid, _)) = read_pid_file(runtime_dir)
        && stored_pid == std::process::id() as i32
    {
        let _ = fs::remove_file(&pid_path);
    }
}

/// Check if a daemon is currently running by reading the PID file and
/// sending signal 0 (no-op) to the process.
#[allow(dead_code)]
pub(crate) fn is_daemon_running(runtime_dir: &Path) -> bool {
    match read_pid_file(runtime_dir) {
        Some((pid, _)) => unsafe { libc::kill(pid, 0) == 0 },
        None => false,
    }
}

/// A cached recipe with its cookbook's priority, used for global sorting.
struct SortableCachedRecipe {
    cached: CachedRecipe,
    priority: u32,
}

/// Serialize a per-cookbook recipe state map into the cache file's
/// JSON-lines format, sorted globally by (sort_order, cookbook priority, name).
pub(crate) fn build_cache_content(state: &HashMap<String, CookbookEntry>) -> String {
    let mut all_recipes: Vec<SortableCachedRecipe> = Vec::new();
    for (cookbook_name, entry) in state {
        for recipe in &entry.recipes {
            all_recipes.push(SortableCachedRecipe {
                cached: CachedRecipe {
                    cookbook: cookbook_name.clone(),
                    name: recipe.name.clone(),
                    description: recipe.description.clone(),
                    sort_order: recipe.sort_order,
                    scores: None,
                },
                priority: entry.priority,
            });
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

/// Parse a JSONL workspace switch event line.
/// Returns `Some((env_name, timestamp))` if the line is a valid `workspace_switch` event,
/// or `None` for any other content (wrong type, missing fields, invalid JSON).
pub(crate) fn parse_switch_event(line: &str) -> Option<(String, i64)> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("type")?.as_str()? != "workspace_switch" {
        return None;
    }
    let env_name = v.get("env_name")?.as_str()?.to_string();
    let timestamp = v.get("timestamp")?.as_i64()?;
    Some((env_name, timestamp))
}

/// How often the daemon's main loop wakes to check the termination flag
/// and drain the cookbook/adapter stdout channel.
const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Run the daemon. Blocks until SIGTERM/SIGINT/SIGHUP.
///
/// `workspaces_directory` is the root under which enwiro environments live;
/// switch events emitted by the adapter `listen` subprocess are resolved as
/// `<workspaces_directory>/<env_name>` and passed to `on_workspace_switch`.
pub fn run(
    workspaces_directory: PathBuf,
    on_workspace_switch: impl Fn(&Path, i64) + Send + 'static,
) -> anyhow::Result<()> {
    let setsid_result = unsafe { libc::setsid() };
    if setsid_result == -1 {
        tracing::warn!("setsid() failed, continuing anyway");
    }

    let dir = runtime_dir()?;
    fs::create_dir_all(&dir)?;

    write_pid_file(&dir)?;

    let term = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&term))?;
    signal_hook::flag::register(signal_hook::consts::SIGHUP, Arc::clone(&term))?;

    let rpc_socket_path = dir.join(enwiro_sdk::rpc::SOCKET_FILENAME);
    let rpc_state = Arc::new(rpc::State::default());
    {
        let rpc_socket_path = rpc_socket_path.clone();
        let rpc_state = rpc_state.clone();
        std::thread::Builder::new()
            .name("enwiro-rpc".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!(error = %e, "rpc tokio runtime build failed");
                        return;
                    }
                };
                if let Err(e) = rt.block_on(rpc::serve(rpc_socket_path, rpc_state)) {
                    tracing::error!(error = %e, "rpc server exited with error");
                }
            })
            .context("spawn rpc thread")?;
    }
    // SAFETY: setenv is documented as unsafe in Rust 2024 because it can
    // race with concurrent reads; we are still single-threaded at this
    // point (signal handlers don't observe envp), so the modification is
    // safe in practice.
    unsafe {
        std::env::set_var(
            enwiro_sdk::rpc::SOCKET_ENV_VAR,
            rpc_socket_path.as_os_str(),
        );
    }

    let (stream_tx, stream_rx) = std::sync::mpsc::channel::<StreamItem>();
    let mut pool = ProcessPool::new(stream_tx);
    let mut recipe_state: HashMap<String, CookbookEntry> = HashMap::new();
    let mut last_cache_content: Option<String> = None;

    let mut desired: Vec<ProcessSource> = Vec::new();
    if let Some(plugin) = get_plugins(PluginKind::Adapter).into_iter().next() {
        desired.push(ProcessSource {
            identity: ProcessIdentity {
                bin: plugin.executable.clone(),
                key: "adapter".to_string(),
            },
            args: vec![
                "listen".to_string(),
                "--debounce-secs".to_string(),
                "5".to_string(),
            ],
            env: Default::default(),
            current_dir: None,
            props: None,
        });
    }
    for plugin in get_plugins(PluginKind::Cookbook) {
        let name = plugin.name.clone();
        let executable = plugin.executable.clone();
        let client = CookbookClient::new_user_level_only(plugin);
        let payload = CookbookPayload::new(client.config().clone());
        let props = serde_json::to_value(&payload).ok();
        recipe_state.insert(
            name.clone(),
            CookbookEntry {
                priority: client.priority(),
                recipes: Vec::new(),
            },
        );
        desired.push(ProcessSource {
            identity: ProcessIdentity {
                bin: executable,
                key: name,
            },
            args: vec!["listen".to_string()],
            env: Default::default(),
            current_dir: None,
            props,
        });
    }

    for (key, err) in pool.reconcile(desired.clone()) {
        tracing::warn!(key = ?key, error = ?err, "Could not spawn listen subprocess");
    }

    tracing::info!(pid = std::process::id(), "Daemon started");

    loop {
        // Reconcile each tick so the upstream pool can restart any
        // listen subprocess that exited since the previous iteration
        // (via `Lifecycle::reconcile_self`, which checks `try_wait`).
        for (key, err) in pool.reconcile(desired.clone()) {
            tracing::warn!(key = ?key, error = ?err, "Could not respawn listen subprocess");
        }

        loop {
            match stream_rx.try_recv() {
                Ok(item) => {
                    if item.stream != StreamKind::Stdout {
                        continue;
                    }
                    if item.key.key == "adapter" {
                        if let Some((env_name, timestamp)) = parse_switch_event(&item.line)
                            && !env_name.is_empty()
                        {
                            let env_dir = workspaces_directory.join(&env_name);
                            on_workspace_switch(&env_dir, timestamp);
                        }
                    } else if let Some(entry) = recipe_state.get_mut(&item.key.key) {
                        match serde_json::from_str::<RecipeUpdate>(&item.line) {
                            Ok(RecipeUpdate::Recipes { data }) => {
                                entry.recipes = data;
                                let new_cache = build_cache_content(&recipe_state);
                                if last_cache_content.as_deref() != Some(new_cache.as_str()) {
                                    if let Err(e) = write_cache_atomic(&dir, &new_cache) {
                                        tracing::error!(error = %e, "Failed to write cache");
                                    }
                                    last_cache_content = Some(new_cache);
                                }
                            }
                            Err(e) => {
                                tracing::debug!(cookbook = %item.key.key, error = %e, line = %item.line, "Could not parse RecipeUpdate from cookbook stdout");
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }

        if term.load(Ordering::Relaxed) {
            tracing::info!("Received termination signal, exiting");
            drop(pool);
            remove_pid_file(&dir);
            let _ = std::fs::remove_file(&rpc_socket_path);
            return Ok(());
        }
        std::thread::sleep(TICK_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enwiro_sdk::client::CachedRecipe;

    fn parse_cached_lines(output: &str) -> Vec<CachedRecipe> {
        output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn entry(priority: u32, recipes: Vec<Recipe>) -> CookbookEntry {
        CookbookEntry { priority, recipes }
    }

    fn recipe_with_desc(name: &str, description: Option<&str>) -> Recipe {
        match description {
            Some(d) => Recipe::with_description(name, d),
            None => Recipe::new(name),
        }
    }

    #[test]
    fn build_cache_content_includes_description() {
        let mut state = HashMap::new();
        state.insert(
            "github".to_string(),
            entry(
                30,
                vec![recipe_with_desc("owner/repo#42", Some("Fix auth bug"))],
            ),
        );
        let entries = parse_cached_lines(&build_cache_content(&state));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].cookbook, "github");
        assert_eq!(entries[0].name, "owner/repo#42");
        assert_eq!(entries[0].description.as_deref(), Some("Fix auth bug"));
    }

    #[test]
    fn build_cache_content_omits_description_when_none() {
        let mut state = HashMap::new();
        state.insert("git".to_string(), entry(10, vec![Recipe::new("repo-a")]));
        let entries = parse_cached_lines(&build_cache_content(&state));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "repo-a");
        assert!(entries[0].description.is_none());
    }

    #[test]
    fn build_cache_content_formats_output_as_jsonl() {
        let mut state = HashMap::new();
        state.insert(
            "git".to_string(),
            entry(10, vec![Recipe::new("repo-a"), Recipe::new("repo-b")]),
        );
        let entries = parse_cached_lines(&build_cache_content(&state));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "repo-a");
        assert_eq!(entries[1].name, "repo-b");
    }

    #[test]
    fn build_cache_content_combines_multiple_cookbooks() {
        let mut state = HashMap::new();
        state.insert("git".to_string(), entry(10, vec![Recipe::new("repo-a")]));
        state.insert("npm".to_string(), entry(20, vec![Recipe::new("pkg-x")]));
        let entries = parse_cached_lines(&build_cache_content(&state));
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"repo-a"));
        assert!(names.contains(&"pkg-x"));
    }

    #[test]
    fn build_cache_content_empty_state_produces_empty_string() {
        let state: HashMap<String, CookbookEntry> = HashMap::new();
        assert_eq!(build_cache_content(&state), "");
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

    fn write_test_pid_file(dir: &Path, pid: i32) {
        std::fs::write(dir.join("daemon.pid"), pid.to_string()).unwrap();
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
    fn test_remove_pid_file_does_not_remove_other_pid() {
        let dir = tempfile::tempdir().unwrap();
        write_test_pid_file(dir.path(), 999999);
        remove_pid_file(dir.path());
        assert!(
            dir.path().join("daemon.pid").exists(),
            "Should not remove another process's PID file"
        );
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
    fn test_read_cache_returns_content_regardless_of_mtime() {
        let dir = tempfile::tempdir().unwrap();
        write_cache_atomic(dir.path(), r#"{"cookbook":"git","name":"old-repo"}"#).unwrap();
        let past = filetime::FileTime::from_system_time(
            std::time::SystemTime::now() - std::time::Duration::from_secs(600),
        );
        filetime::set_file_mtime(dir.path().join("recipes.cache"), past).unwrap();
        let read = read_cached_recipes(dir.path()).unwrap();
        assert!(
            read.is_some(),
            "Cache freshness is no longer time-based — listen-driven cookbooks may not emit for arbitrary periods"
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
    fn build_cache_content_sorts_by_priority() {
        let mut state = HashMap::new();
        state.insert(
            "github".to_string(),
            entry(30, vec![Recipe::new("repo#42")]),
        );
        state.insert("git".to_string(), entry(10, vec![Recipe::new("my-repo")]));
        let entries = parse_cached_lines(&build_cache_content(&state));
        assert_eq!(
            entries[0].cookbook, "git",
            "Higher priority (lower number) should come first"
        );
        assert_eq!(entries[0].name, "my-repo");
        assert_eq!(entries[1].cookbook, "github");
        assert_eq!(entries[1].name, "repo#42");
    }

    #[test]
    fn build_cache_content_sorts_by_name_on_priority_tie() {
        let mut state = HashMap::new();
        state.insert("npm".to_string(), entry(20, vec![Recipe::new("pkg-x")]));
        state.insert("git".to_string(), entry(20, vec![Recipe::new("repo-a")]));
        let entries = parse_cached_lines(&build_cache_content(&state));
        assert_eq!(
            entries[0].name, "pkg-x",
            "Same sort_order and priority should tie-break by recipe name"
        );
        assert_eq!(entries[0].cookbook, "npm");
        assert_eq!(entries[1].name, "repo-a");
        assert_eq!(entries[1].cookbook, "git");
    }

    #[test]
    fn build_cache_content_sorts_globally_by_sort_order() {
        let mut git_recipes = vec![Recipe::new("git-repo-a"), Recipe::new("git-repo-b")];
        git_recipes[0].sort_order = 0;
        git_recipes[1].sort_order = 50;
        let mut gh_recipes = vec![Recipe::new("gh-issue-1"), Recipe::new("gh-issue-2")];
        gh_recipes[0].sort_order = 0;
        gh_recipes[1].sort_order = 50;

        let mut state = HashMap::new();
        state.insert("git".to_string(), entry(10, git_recipes));
        state.insert("github".to_string(), entry(30, gh_recipes));
        let entries = parse_cached_lines(&build_cache_content(&state));
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].name, "git-repo-a");
        assert_eq!(entries[0].sort_order, 0);
        assert_eq!(entries[1].name, "gh-issue-1");
        assert_eq!(entries[1].sort_order, 0);
        assert_eq!(entries[2].name, "git-repo-b");
        assert_eq!(entries[2].sort_order, 50);
        assert_eq!(entries[3].name, "gh-issue-2");
        assert_eq!(entries[3].sort_order, 50);
    }

    #[test]
    fn test_parse_switch_event_valid_line() {
        let line = r#"{"type":"workspace_switch","timestamp":1700000000,"env_name":"my-project"}"#;
        let result = parse_switch_event(line);
        assert_eq!(
            result,
            Some(("my-project".to_string(), 1700000000i64)),
            "valid switch event line must parse to (env_name, timestamp)"
        );
    }

    #[test]
    fn test_parse_switch_event_wrong_type_returns_none() {
        let line = r#"{"type":"other_event","timestamp":1700000000,"env_name":"my-project"}"#;
        let result = parse_switch_event(line);
        assert_eq!(
            result, None,
            "event with type != 'workspace_switch' must return None"
        );
    }

    #[test]
    fn test_parse_switch_event_missing_env_name_returns_none() {
        let line = r#"{"type":"workspace_switch","timestamp":1700000000}"#;
        let result = parse_switch_event(line);
        assert_eq!(result, None, "line without env_name field must return None");
    }

    #[test]
    fn test_parse_switch_event_missing_timestamp_returns_none() {
        let line = r#"{"type":"workspace_switch","env_name":"my-project"}"#;
        let result = parse_switch_event(line);
        assert_eq!(
            result, None,
            "line without timestamp field must return None"
        );
    }

    #[test]
    fn test_parse_switch_event_invalid_json_returns_none() {
        let line = "this is not json at all";
        let result = parse_switch_event(line);
        assert_eq!(
            result, None,
            "invalid JSON must return None rather than panic"
        );
    }

    #[test]
    fn test_parse_switch_event_empty_string_returns_none() {
        let result = parse_switch_event("");
        assert_eq!(result, None, "empty string must return None");
    }

    #[test]
    fn test_parse_switch_event_empty_env_name_is_returned() {
        let line = r#"{"type":"workspace_switch","timestamp":1700000000,"env_name":""}"#;
        let result = parse_switch_event(line);
        assert_eq!(
            result,
            Some(("".to_string(), 1700000000i64)),
            "empty env_name must still be returned; filtering is the caller's job"
        );
    }

    #[test]
    fn test_parse_switch_event_extra_fields_are_ignored() {
        let line = r#"{"type":"workspace_switch","timestamp":1700000000,"env_name":"proj","extra":"ignored","count":42}"#;
        let result = parse_switch_event(line);
        assert_eq!(
            result,
            Some(("proj".to_string(), 1700000000i64)),
            "extra unknown fields must not prevent parsing"
        );
    }

    #[test]
    fn build_cache_content_interleaves_cookbooks_by_sort_order() {
        let mut git_recipe = Recipe::new("low-priority-branch");
        git_recipe.sort_order = 80;
        let mut gh_recipe = Recipe::new("hot-issue");
        gh_recipe.sort_order = 0;

        let mut state = HashMap::new();
        state.insert("git".to_string(), entry(10, vec![git_recipe]));
        state.insert("github".to_string(), entry(30, vec![gh_recipe]));
        let entries = parse_cached_lines(&build_cache_content(&state));
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].name, "hot-issue",
            "Lower sort_order should come first regardless of cookbook priority"
        );
        assert_eq!(entries[1].name, "low-priority-branch");
    }
}
