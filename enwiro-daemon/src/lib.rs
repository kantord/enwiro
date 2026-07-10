//! Background daemon for enwiro.
//!
//! Refreshes the cookbook recipe cache on a periodic interval (when the user
//! is active) and forwards workspace switch events from the adapter's
//! `listen` subcommand. The caller provides a `workspaces_directory` and a
//! `on_workspace_switch` callback; everything else (PID file, signal
//! handling, idle detection, cache file location) is owned by this crate.

pub mod config;
pub mod launch;
pub mod meta;
#[cfg(feature = "container-wrap")]
pub mod proxy;
pub mod rpc;
pub mod scoring;
pub use config::ConfigurationValues;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use tokio::signal::unix::{SignalKind, signal};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use enwiro_sdk::adapter::AdapterCapability;
use enwiro_sdk::bridge::BridgeCapability;
use enwiro_sdk::client::{CachedPatternRecipe, CachedRecipe, CookbookClient, CookbookTrait};
use enwiro_sdk::cookbook::{CookbookCapability, CookbookPayload, RecipeItem};
use enwiro_sdk::listen::RecipeUpdate;
use enwiro_sdk::plugin::{PluginKind, get_plugins};
use optative_process_pool::{ProcessIdentity, ProcessPool, ProcessSource, StreamItem, StreamKind};

/// Per-cookbook state assembled from the listen-stdout stream. The
/// cache content is rebuilt from this map on every change.
struct CookbookEntry {
    priority: u32,
    recipes: Vec<RecipeItem>,
}

/// Caller-provided startup configuration for `run`. Distinct from
/// `config::ConfigurationValues`, which carries project-level cookbook
/// config; this struct only carries what the daemon needs to start.
pub struct DaemonConfig {
    /// Root directory under which env worktrees live; switch events name
    /// envs by basename, resolved as `workspaces_directory/<env_name>`.
    pub workspaces_directory: PathBuf,
    /// OCI runtime for container launches (issue #540); see
    /// `config::ConfigurationValues::container_runtime`.
    pub container_runtime: Option<String>,
    /// Name of the adapter whose `listen` subcommand feeds switch events
    /// (`config::ConfigurationValues::adapter`). `None` means no adapter
    /// was configured or auto-selected.
    pub adapter: Option<String>,
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

/// Acquire an exclusive non-blocking lock on `<runtime_dir>/daemon.lock`.
/// Returns the lock file handle — the lock is held for as long as the
/// returned file is alive (kernel releases it on `close(2)`). If another
/// daemon already holds the lock, returns an error before we touch the
/// socket file, preventing two instances from racing over `bind`.
pub(crate) fn acquire_daemon_lock(runtime_dir: &Path) -> anyhow::Result<std::fs::File> {
    use std::os::unix::io::AsRawFd;

    fs::create_dir_all(runtime_dir).context("Could not create runtime directory for lock")?;
    let lock_path = runtime_dir.join("daemon.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open daemon lock at {}", lock_path.display()))?;

    // flock(LOCK_EX | LOCK_NB): exclusive, non-blocking. Fails fast with
    // EWOULDBLOCK if another daemon holds the lock.
    let fd = file.as_raw_fd();
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        let errno = std::io::Error::last_os_error();
        anyhow::bail!(
            "another enwiro-daemon instance is already running (could not acquire \
             exclusive lock on {}: {}). Stop it with `systemctl --user stop \
             enwiro-daemon.service` (or the equivalent) before starting another.",
            lock_path.display(),
            errno
        );
    }
    Ok(file)
}

/// A cached recipe with its cookbook's priority, used for global sorting.
struct SortableCachedRecipe {
    cached: CachedRecipe,
    priority: u32,
}

/// Serialize a per-cookbook recipe state map into the cache file's
/// JSON-lines format: concrete recipes sorted globally by (sort_order,
/// cookbook priority, name), then pattern claims sorted by (cookbook
/// priority, pattern). Patterns are validated and anchored here
/// (`enwiro_sdk::recipe_pattern`), invalid ones dropped - consumers never see a
/// pattern that doesn't compile or whose template keys don't resolve.
pub(crate) fn build_cache_content(state: &HashMap<String, CookbookEntry>) -> String {
    let mut all_recipes: Vec<SortableCachedRecipe> = Vec::new();
    let mut all_patterns: Vec<(u32, CachedPatternRecipe)> = Vec::new();
    for (cookbook_name, entry) in state {
        for item in &entry.recipes {
            match item {
                RecipeItem::Concrete(recipe) => all_recipes.push(SortableCachedRecipe {
                    cached: cached_concrete_entry(cookbook_name, recipe),
                    priority: entry.priority,
                }),
                RecipeItem::Pattern(pattern) => {
                    if let Some(cached) = cached_pattern_entry(cookbook_name, pattern) {
                        all_patterns.push((entry.priority, cached));
                    }
                }
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
    all_patterns.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.pattern.cmp(&b.1.pattern)));

    let mut output = String::new();
    for item in all_recipes {
        if let Ok(json) = serde_json::to_string(&item.cached) {
            output.push_str(&json);
            output.push('\n');
        }
    }
    for (_, pattern) in all_patterns {
        if let Ok(json) = serde_json::to_string(&pattern) {
            output.push_str(&json);
            output.push('\n');
        }
    }
    output
}

fn cached_concrete_entry(cookbook: &str, recipe: &enwiro_sdk::Recipe) -> CachedRecipe {
    CachedRecipe {
        cookbook: cookbook.to_string(),
        name: recipe.name.clone(),
        description: recipe
            .description
            .as_deref()
            .map(enwiro_sdk::recipe_pattern::truncate_description),
        sort_order: recipe.sort_order,
        equivalent_to: recipe.equivalent_to.clone(),
        scores: None,
    }
}

/// Validate and anchor one cookbook-emitted pattern claim. `None` (with a
/// warning) drops the entry before any consumer sees it. The description
/// TEMPLATE is stored exactly as validated - truncating it could cut
/// through a `{key}` and store an unparseable template; length-capping is
/// the renderer's job (`match_name` truncates the rendered output).
fn cached_pattern_entry(
    cookbook: &str,
    pattern: &enwiro_sdk::PatternRecipe,
) -> Option<CachedPatternRecipe> {
    if let Err(e) =
        enwiro_sdk::recipe_pattern::validate(&pattern.pattern, pattern.description.as_deref())
    {
        tracing::warn!(
            cookbook = %cookbook,
            pattern = %pattern.pattern,
            error = %e,
            "Dropping invalid pattern recipe"
        );
        return None;
    }
    Some(CachedPatternRecipe {
        cookbook: cookbook.to_string(),
        pattern: enwiro_sdk::recipe_pattern::anchor(&pattern.pattern),
        description: pattern.description.clone(),
    })
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

/// Namespace prefix for bridge process keys in the pool, so a bridge can
/// never collide with a cookbook of the same name in the stream dispatch.
const BRIDGE_KEY_PREFIX: &str = "bridge:";

/// The shared shape of every supervised `listen` subprocess; only the
/// binary, pool key, argv, and stdin payload differ per plugin kind.
fn listen_source(
    bin: String,
    key: String,
    args: Vec<String>,
    props: Option<serde_json::Value>,
) -> ProcessSource {
    ProcessSource {
        identity: ProcessIdentity { bin, key },
        args,
        env: Default::default(),
        current_dir: None,
        props,
    }
}

/// Run `probe` against every plugin concurrently, one thread each, and
/// return the results in input order. Each metadata probe can block for
/// its full timeout on a broken plugin, so probing serially would make
/// worst-case daemon startup degrade linearly with the number of
/// installed plugins; concurrently it is bounded by the slowest single
/// probe.
fn probe_concurrently<T: Send>(
    plugins: Vec<enwiro_sdk::plugin::Plugin>,
    probe: impl Fn(enwiro_sdk::plugin::Plugin) -> T + Sync,
) -> Vec<T> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = plugins
            .into_iter()
            .map(|plugin| scope.spawn(|| probe(plugin)))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("metadata probe thread panicked"))
            .collect()
    })
}

/// Process sources for bridges that declare the `listen` capability in
/// their `metadata` output. Bridges that don't (probe failure, timeout,
/// no recognized capability) are left alone. The match is exhaustive on
/// the kind's capability enum: a capability added to the SDK won't compile
/// here until the daemon decides how to handle it.
fn bridge_listen_sources(
    bridges: impl IntoIterator<Item = enwiro_sdk::plugin::Plugin>,
) -> Vec<ProcessSource> {
    let probed = probe_concurrently(bridges.into_iter().collect(), |plugin| {
        let metadata = enwiro_sdk::bridge::fetch_bridge_metadata(&plugin.executable);
        (plugin, metadata)
    });
    let mut sources = Vec::new();
    for (plugin, metadata) in probed {
        let recognized: Vec<BridgeCapability> = metadata.capabilities.recognized().collect();
        if recognized.is_empty() {
            tracing::debug!(bridge = %plugin.name, "Bridge declares no recognized capabilities, leaving it alone");
        }
        for capability in recognized {
            match capability {
                BridgeCapability::Listen => {
                    tracing::info!(bridge = %plugin.name, "Bridge declares listen capability, autostarting");
                    sources.push(listen_source(
                        plugin.executable.clone(),
                        format!("{BRIDGE_KEY_PREFIX}{}", plugin.name),
                        vec!["listen".to_string()],
                        None,
                    ));
                }
            }
        }
    }
    sources
}

/// Register every cookbook in the recipe state and return listen sources
/// for the ones that declare the `listen` capability. A cookbook without
/// it is still a full citizen for RPC-driven subcommands (`cook`, `gear`,
/// ...) - the daemon just doesn't spawn anything for it.
fn cookbook_sources(
    cookbooks: impl IntoIterator<Item = enwiro_sdk::plugin::Plugin>,
    recipe_state: &mut HashMap<String, CookbookEntry>,
) -> Vec<ProcessSource> {
    // Client construction runs the metadata probe subprocess; parallelize
    // it, then do the (cheap, shared-state) registration serially.
    let clients = probe_concurrently(cookbooks.into_iter().collect(), |plugin| {
        let name = plugin.name.to_string();
        let executable = plugin.executable.clone();
        (
            name,
            executable,
            CookbookClient::new_user_level_only(plugin),
        )
    });
    let mut sources = Vec::new();
    for (name, executable, client) in clients {
        recipe_state.insert(
            name.clone(),
            CookbookEntry {
                priority: client.priority(),
                recipes: Vec::new(),
            },
        );
        let recognized: Vec<CookbookCapability> =
            client.metadata().capabilities.recognized().collect();
        if recognized.is_empty() {
            tracing::debug!(cookbook = %name, "Cookbook declares no recognized capabilities, not spawning listen");
        }
        for capability in recognized {
            match capability {
                CookbookCapability::Listen => {
                    tracing::info!(cookbook = %name, "Cookbook declares listen capability, autostarting");
                    let payload = CookbookPayload::new(client.config().clone());
                    sources.push(listen_source(
                        executable.clone(),
                        name.clone(),
                        vec!["listen".to_string()],
                        serde_json::to_value(&payload).ok(),
                    ));
                }
            }
        }
    }
    sources
}

/// Listen source for the configured adapter, if it declares the `listen`
/// capability in its `metadata` output. Adapters predating the metadata
/// convention probe to no capabilities and are left alone - the daemon
/// then runs without switch events rather than crash-looping the adapter.
fn adapter_listen_source(plugin: &enwiro_sdk::plugin::Plugin) -> Option<ProcessSource> {
    let metadata = enwiro_sdk::adapter::fetch_adapter_metadata(&plugin.executable);
    let source = metadata
        .capabilities
        .recognized()
        .map(|capability| match capability {
            AdapterCapability::Listen => listen_source(
                plugin.executable.clone(),
                "adapter".to_string(),
                vec!["listen".to_string()],
                None,
            ),
        })
        .next();
    match &source {
        Some(_) => tracing::info!(adapter = %plugin.name, "Spawning adapter listen subprocess"),
        None => tracing::warn!(
            adapter = %plugin.name,
            "Configured adapter does not declare the listen capability; switch events disabled"
        ),
    }
    source
}

/// Resolve the adapter whose `listen` subcommand feeds switch events.
/// Choosing the name is `ConfigurationValues`' job alone (explicit config,
/// or its single-installed-adapter auto-select); this only resolves the
/// chosen name to a binary and explains why events are off when it can't.
/// The daemon used to ignore the choice and spawn an arbitrary installed
/// adapter, silently starving switch consumers when the wrong one won.
fn select_listen_adapter(configured: Option<&str>) -> Option<enwiro_sdk::plugin::Plugin> {
    let Some(name) = configured else {
        tracing::warn!(
            "No adapter configured or auto-selected; switch events disabled. \
             Set `adapter` in enwiro.toml if more than one adapter is installed."
        );
        return None;
    };
    let found = enwiro_sdk::plugin::get_plugin_by_name(PluginKind::Adapter, name);
    if found.is_none() {
        tracing::warn!(
            adapter = name,
            "Configured adapter is not installed; switch events disabled"
        );
    }
    found
}

/// Run the daemon. Awaits until SIGTERM/SIGINT/SIGHUP.
///
/// `config.workspaces_directory` is the root under which enwiro environments
/// live; switch events emitted by the adapter `listen` subprocess are
/// resolved as `<workspaces_directory>/<env_name>` and passed to
/// `on_workspace_switch`.
pub async fn run(
    config: DaemonConfig,
    on_workspace_switch: impl Fn(&Path, i64) + Send + 'static,
) -> anyhow::Result<()> {
    let DaemonConfig {
        workspaces_directory,
        container_runtime,
        adapter,
    } = config;

    let setsid_result = unsafe { libc::setsid() };
    if setsid_result == -1 {
        tracing::warn!("setsid() failed, continuing anyway");
    }

    let dir = runtime_dir()?;
    fs::create_dir_all(&dir)?;

    // Kernel-mutex against double-start. Held for the lifetime of `run()`
    // via this binding; closing the file (drop) releases the lock.
    let _daemon_lock = acquire_daemon_lock(&dir)?;

    write_pid_file(&dir)?;

    let rpc_socket_path = dir.join(enwiro_sdk::rpc::SOCKET_FILENAME);
    // SAFETY (Rust 2024): set_var can race with concurrent env reads.
    // Done before any tokio task or child spawn, so the mutation is sound.
    unsafe {
        std::env::set_var(enwiro_sdk::rpc::SOCKET_ENV_VAR, rpc_socket_path.as_os_str());
    }

    let active_env: rpc::SharedActiveEnv = Arc::new(Mutex::new(None));
    let active_env_writer = active_env.clone();
    tokio::spawn(rpc::serve(
        rpc_socket_path.clone(),
        active_env,
        workspaces_directory.clone(),
        container_runtime,
    ));

    // Host-side Claude auth proxy: keeps the OAuth token off the container.
    #[cfg(feature = "container-wrap")]
    tokio::spawn(proxy::serve());

    let (stream_tx, stream_rx) = std::sync::mpsc::channel::<StreamItem>();
    let mut pool = ProcessPool::new(stream_tx);
    let mut recipe_state: HashMap<String, CookbookEntry> = HashMap::new();
    let mut last_cache_content: Option<String> = None;

    let mut desired: Vec<ProcessSource> = Vec::new();
    if let Some(plugin) = select_listen_adapter(adapter.as_deref()) {
        desired.extend(adapter_listen_source(&plugin));
    }
    desired.extend(cookbook_sources(
        get_plugins(PluginKind::Cookbook),
        &mut recipe_state,
    ));
    desired.extend(bridge_listen_sources(get_plugins(PluginKind::Bridge)));

    for (key, err) in pool.reconcile(desired.clone()) {
        tracing::warn!(key = ?key, error = ?err, "Could not spawn listen subprocess");
    }

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sighup = signal(SignalKind::hangup())?;
    let mut tick = tokio::time::interval(TICK_INTERVAL);

    tracing::info!(pid = std::process::id(), "Daemon started");

    loop {
        tokio::select! {
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
            _ = sighup.recv() => break,
            _ = tick.tick() => {
                for (key, err) in pool.reconcile(desired.clone()) {
                    tracing::warn!(key = ?key, error = ?err, "Could not respawn listen subprocess");
                }
                while let Ok(item) = stream_rx.try_recv() {
                    if item.stream != StreamKind::Stdout {
                        continue;
                    }
                    if item.key.key == "adapter" {
                        if let Some((env_name, timestamp)) = parse_switch_event(&item.line)
                            && !env_name.is_empty()
                        {
                            *active_env_writer.lock().unwrap() =
                                Some(rpc::ActiveEnvState { env_name: env_name.clone(), timestamp });
                            on_workspace_switch(&workspaces_directory.join(&env_name), timestamp);
                        }
                    } else if let Some(bridge) = item.key.key.strip_prefix(BRIDGE_KEY_PREFIX) {
                        // Bridges emit no events the daemon acts on; their
                        // stdout is surfaced as a log channel only.
                        tracing::info!(bridge, line = %item.line, "bridge output");
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
                            Ok(RecipeUpdate::StatusChanged { recipe, status }) => {
                                apply_auto_status(
                                    &workspaces_directory,
                                    &item.key.key,
                                    &recipe,
                                    status,
                                );
                            }
                            Err(e) => {
                                tracing::debug!(cookbook = %item.key.key, error = %e, line = %item.line, "Could not parse RecipeUpdate from cookbook stdout");
                            }
                        }
                    }
                }
            }
        }
    }

    tracing::info!("Received termination signal, exiting");
    drop(pool);
    remove_pid_file(&dir);
    let _ = std::fs::remove_file(&rpc_socket_path);
    Ok(())
}

/// Env dir for a recipe: stored under the recipe name with `/` flattened to `-`.
fn env_dir_for_recipe(workspaces_directory: &Path, recipe: &str) -> PathBuf {
    workspaces_directory.join(recipe.replace('/', "-"))
}

/// Only the cookbook that owns an env may set its status. An env with no
/// recorded cookbook is owned by no one and so settable by any.
fn cookbook_owns_env(meta: &crate::meta::EnvStats, cookbook: &str) -> bool {
    meta.cookbook.as_deref().is_none_or(|c| c == cookbook)
}

/// Whether `recipe` matches the env's recorded recipe. `recipe` is only
/// recorded since #325, so pre-#325 envs (`recipe: None`) match by env-dir
/// name + ownership alone.
/// TODO(legacy, revisit after 2026-06): once pre-#325 envs have aged out,
/// tighten this to require `meta.recipe == Some(recipe)`.
fn recipe_matches_env(meta: &crate::meta::EnvStats, recipe: &str) -> bool {
    meta.recipe.as_deref().is_none_or(|r| r == recipe)
}

/// Manual override wins: true when the most recent status change was user-set,
/// so an auto-status must not overwrite it.
fn last_status_change_is_user_set(meta: &crate::meta::EnvStats) -> bool {
    meta.event_log
        .iter()
        .rev()
        .find(|e| e.event_type == crate::meta::EventType::StatusChange)
        .and_then(|e| e.set_by.as_ref())
        .is_some_and(crate::meta::StatusSource::is_user)
}

/// Event-log label for a cookbook-settable status, or `None` for one a
/// cookbook may not set.
fn auto_status_label(status: &crate::meta::Status) -> Option<&'static str> {
    match status {
        crate::meta::Status::Done { .. } => Some("done"),
        crate::meta::Status::Evergreen => Some("evergreen"),
        _ => None,
    }
}

/// Write a cookbook-reported auto-status into a loaded env `meta` and persist it.
fn write_auto_status(
    env_dir: &Path,
    recipe: &str,
    cookbook: &str,
    mut meta: crate::meta::EnvStats,
    status: crate::meta::Status,
    label: &str,
) {
    let now = crate::meta::now_utc();
    meta.status = Some(status);
    meta.event_log.push(crate::meta::EventLogEntry {
        event_type: crate::meta::EventType::StatusChange,
        detail: label.to_string(),
        set_by: Some(crate::meta::StatusSource::Auto {
            cookbook: Some(cookbook.to_string()),
        }),
        started: now,
        ended: Some(now),
    });
    if let Err(e) = crate::meta::save_env_meta(env_dir, &meta) {
        tracing::error!(error = %e, recipe, "failed to save auto status");
    }
}

/// Apply a cookbook-reported auto-status to the recipe's env `meta.json`.
/// Best-effort: silently ignores statuses a cookbook may not set, recipes
/// with no matching env, and envs whose status the user set manually.
fn apply_auto_status(
    workspaces_directory: &Path,
    cookbook: &str,
    recipe: &str,
    status: crate::meta::Status,
) {
    if !crate::meta::is_cookbook_settable(&status) {
        tracing::debug!(recipe, "ignoring auto status a cookbook may not set");
        return;
    }

    let env_dir = env_dir_for_recipe(workspaces_directory, recipe);
    if !env_dir.is_dir() {
        return;
    }

    let meta = crate::meta::load_env_meta(&env_dir);
    if !cookbook_owns_env(&meta, cookbook) {
        return;
    }
    if !recipe_matches_env(&meta, recipe) {
        return;
    }
    if last_status_change_is_user_set(&meta) {
        return;
    }
    // No-op if unchanged, to avoid log spam.
    if meta.status.as_ref() == Some(&status) {
        return;
    }

    // unreachable given is_cookbook_settable above, but stay defensive.
    let Some(label) = auto_status_label(&status) else {
        return;
    };
    write_auto_status(&env_dir, recipe, cookbook, meta, status, label);
}

#[cfg(test)]
mod tests {
    use super::*;
    use enwiro_sdk::client::CachedRecipe;
    use enwiro_sdk::cookbook::Recipe;

    fn parse_cached_lines(output: &str) -> Vec<CachedRecipe> {
        output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn entry(priority: u32, recipes: Vec<Recipe>) -> CookbookEntry {
        CookbookEntry {
            priority,
            recipes: recipes.into_iter().map(RecipeItem::from).collect(),
        }
    }

    fn recipe_with_desc(name: &str, description: Option<&str>) -> Recipe {
        match description {
            Some(d) => Recipe::with_description(name, d),
            None => Recipe::new(name),
        }
    }

    fn pattern_entry(priority: u32, pattern: &str, description: Option<&str>) -> CookbookEntry {
        CookbookEntry {
            priority,
            recipes: vec![RecipeItem::Pattern(enwiro_sdk::cookbook::PatternRecipe {
                pattern: pattern.to_string(),
                description: description.map(str::to_string),
            })],
        }
    }

    fn parse_pattern_lines(output: &str) -> Vec<CachedPatternRecipe> {
        output
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    #[test]
    fn build_cache_content_emits_anchored_pattern_lines_after_concrete_ones() {
        let mut state = HashMap::new();
        state.insert(
            "git".to_string(),
            CookbookEntry {
                priority: 10,
                recipes: vec![
                    Recipe::new("my-project").into(),
                    RecipeItem::Pattern(enwiro_sdk::cookbook::PatternRecipe {
                        pattern: "my-project@(?P<branch>.+)".to_string(),
                        description: Some("Create new branch '{branch}'".to_string()),
                    }),
                ],
            },
        );

        let output = build_cache_content(&state);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2);
        // Concrete first - old consumers read a prefix of the file they
        // fully understand.
        let concrete: CachedRecipe = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(concrete.name, "my-project");
        let pattern: CachedPatternRecipe = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(pattern.pattern, "^(?:my-project@(?P<branch>.+))$");
        assert_eq!(pattern.cookbook, "git");
    }

    #[test]
    fn build_cache_content_drops_pattern_with_invalid_regex() {
        let mut state = HashMap::new();
        state.insert("git".to_string(), pattern_entry(10, "broken(", None));

        let output = build_cache_content(&state);
        assert!(output.is_empty(), "got: {output}");
    }

    #[test]
    fn build_cache_content_drops_pattern_with_unknown_template_key() {
        let mut state = HashMap::new();
        state.insert(
            "git".to_string(),
            pattern_entry(10, "repo@(?P<branch>.+)", Some("branch {typo}")),
        );

        let output = build_cache_content(&state);
        assert!(output.is_empty(), "got: {output}");
    }

    #[test]
    fn build_cache_content_sorts_patterns_by_cookbook_priority() {
        let mut state = HashMap::new();
        state.insert("low".to_string(), pattern_entry(30, "b@(.+)", None));
        state.insert("high".to_string(), pattern_entry(10, "a@(.+)", None));

        let patterns = parse_pattern_lines(&build_cache_content(&state));
        assert_eq!(patterns.len(), 2);
        assert_eq!(patterns[0].cookbook, "high");
        assert_eq!(patterns[1].cookbook, "low");
    }

    #[test]
    fn build_cache_content_stores_long_pattern_templates_intact() {
        // A template cut at the cap could split a `{key}` into an
        // unparseable template; what was validated must be what is stored.
        let template = format!(
            "{}{{branch}} suffix past the cap",
            "d".repeat(enwiro_sdk::recipe_pattern::MAX_DESCRIPTION_CHARS)
        );
        let mut state = HashMap::new();
        state.insert(
            "git".to_string(),
            pattern_entry(10, "p@(?P<branch>.+)", Some(&template)),
        );

        let patterns = parse_pattern_lines(&build_cache_content(&state));
        assert_eq!(patterns[0].description.as_deref(), Some(template.as_str()));
        // ... and the stored entry still renders at match time.
        let matched = enwiro_sdk::recipe_pattern::match_name(
            &patterns[0].pattern,
            patterns[0].description.as_deref(),
            "p@new-idea",
        )
        .unwrap();
        assert!(matched.description.is_some());
    }

    #[test]
    fn build_cache_content_truncates_long_descriptions() {
        let long: String = "d".repeat(500);
        let mut state = HashMap::new();
        state.insert(
            "git".to_string(),
            entry(10, vec![recipe_with_desc("repo", Some(&long))]),
        );

        let entries = parse_cached_lines(&build_cache_content(&state));
        let description = entries[0].description.as_deref().unwrap();
        assert_eq!(
            description.chars().count(),
            enwiro_sdk::recipe_pattern::MAX_DESCRIPTION_CHARS
        );
        assert!(description.ends_with('…'));
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
    fn no_configured_adapter_selects_none() {
        assert!(select_listen_adapter(None).is_none());
    }

    #[test]
    fn configured_but_uninstalled_adapter_selects_none() {
        assert!(select_listen_adapter(Some("definitely-not-installed")).is_none());
    }

    fn fake_bridge(
        dir: &std::path::Path,
        name: &str,
        script_body: &str,
    ) -> enwiro_sdk::plugin::Plugin {
        fake_plugin(dir, PluginKind::Bridge, name, script_body)
    }

    #[test]
    fn bridge_declaring_listen_gets_a_listen_source() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = fake_bridge(
            dir.path(),
            "aw",
            r#"echo '{"capabilities":[{"name":"listen"}]}'"#,
        );
        let sources = bridge_listen_sources([bridge]);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].identity.key, "bridge:aw");
        assert_eq!(sources[0].args, vec!["listen".to_string()]);
    }

    #[test]
    fn bridge_without_listen_capability_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = fake_bridge(dir.path(), "rofi", "echo '{}'");
        assert!(bridge_listen_sources([bridge]).is_empty());
    }

    #[test]
    fn bridge_predating_the_metadata_convention_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = fake_bridge(dir.path(), "legacy", "echo 'rofi row one'");
        assert!(bridge_listen_sources([bridge]).is_empty());
    }

    fn fake_plugin(
        dir: &std::path::Path,
        kind: PluginKind,
        name: &str,
        script_body: &str,
    ) -> enwiro_sdk::plugin::Plugin {
        use std::os::unix::fs::PermissionsExt;
        let prefix = format!("enwiro-{kind}").to_lowercase();
        let path = dir.join(format!("{prefix}-{name}"));
        std::fs::write(&path, format!("#!/bin/sh\n{script_body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        enwiro_sdk::plugin::Plugin {
            name: enwiro_sdk::plugin::PluginName::new(name).unwrap(),
            kind,
            executable: path.to_string_lossy().to_string(),
        }
    }

    #[test]
    fn cookbook_declaring_listen_gets_a_listen_source() {
        let dir = tempfile::tempdir().unwrap();
        let cookbook = fake_plugin(
            dir.path(),
            PluginKind::Cookbook,
            "fake",
            r#"echo '{"capabilities":[{"name":"listen"}]}'"#,
        );
        let mut recipe_state = HashMap::new();
        let sources = cookbook_sources([cookbook], &mut recipe_state);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].identity.key, "fake");
        assert_eq!(sources[0].args, vec!["listen".to_string()]);
        assert!(
            sources[0].props.is_some(),
            "listen sources must carry the config payload"
        );
    }

    #[test]
    fn cookbook_without_listen_is_registered_but_not_spawned() {
        let dir = tempfile::tempdir().unwrap();
        let cookbook = fake_plugin(dir.path(), PluginKind::Cookbook, "fake", "echo '{}'");
        let mut recipe_state = HashMap::new();
        assert!(cookbook_sources([cookbook], &mut recipe_state).is_empty());
        assert!(
            recipe_state.contains_key("fake"),
            "a non-listening cookbook must still be registered for its recipes"
        );
    }

    #[test]
    fn cookbook_predating_the_metadata_convention_is_not_spawned() {
        let dir = tempfile::tempdir().unwrap();
        let cookbook = fake_plugin(dir.path(), PluginKind::Cookbook, "legacy", "exit 2");
        let mut recipe_state = HashMap::new();
        assert!(cookbook_sources([cookbook], &mut recipe_state).is_empty());
    }

    #[test]
    fn adapter_declaring_listen_gets_a_listen_source() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = fake_plugin(
            dir.path(),
            PluginKind::Adapter,
            "fake",
            r#"echo '{"capabilities":[{"name":"listen"}]}'"#,
        );
        let source = adapter_listen_source(&adapter).expect("listen source");
        assert_eq!(source.identity.key, "adapter");
        assert_eq!(source.args, vec!["listen".to_string()]);
    }

    #[test]
    fn adapter_without_listen_capability_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = fake_plugin(dir.path(), PluginKind::Adapter, "fake", "echo '{}'");
        assert!(adapter_listen_source(&adapter).is_none());
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

    mod apply_auto_status_tests {
        use super::super::apply_auto_status;
        use crate::meta::{
            CookedPhase, DoneOutcome, EnvStats, EventLogEntry, EventType, Status, StatusDetail,
            StatusSource, load_env_meta, now_utc, save_env_meta,
        };
        use std::path::{Path, PathBuf};

        /// Create an env dir under `ws` with the given cookbook/recipe metadata.
        fn make_env(
            ws: &Path,
            name: &str,
            cookbook: Option<&str>,
            recipe: Option<&str>,
        ) -> PathBuf {
            let env_dir = ws.join(name);
            std::fs::create_dir_all(&env_dir).unwrap();
            let meta = EnvStats {
                cookbook: cookbook.map(str::to_string),
                recipe: recipe.map(str::to_string),
                ..Default::default()
            };
            save_env_meta(&env_dir, &meta).unwrap();
            env_dir
        }

        /// Seed an existing status + a `StatusChange` event with the given `set_by`.
        fn seed_status(env_dir: &Path, status: Status, set_by: StatusSource) {
            let mut meta = load_env_meta(env_dir);
            meta.status = Some(status);
            let now = now_utc();
            meta.event_log.push(EventLogEntry {
                event_type: EventType::StatusChange,
                detail: "seed".to_string(),
                set_by: Some(set_by),
                started: now,
                ended: Some(now),
            });
            save_env_meta(env_dir, &meta).unwrap();
        }

        fn done() -> Status {
            Status::Done {
                outcome: Some(DoneOutcome::Completed),
            }
        }

        #[test]
        fn happy_path_writes_status_and_provenance() {
            let ws = tempfile::tempdir().unwrap();
            let env = make_env(ws.path(), "jq", Some("git"), Some("jq"));
            apply_auto_status(ws.path(), "git", "jq", Status::Evergreen);
            let meta = load_env_meta(&env);
            assert_eq!(meta.status, Some(Status::Evergreen));
            let last = meta.event_log.last().unwrap();
            assert_eq!(last.event_type, EventType::StatusChange);
            assert_eq!(
                last.set_by,
                Some(StatusSource::Auto {
                    cookbook: Some("git".to_string())
                })
            );
        }

        #[test]
        fn recipe_none_still_matches_legacy_env() {
            let ws = tempfile::tempdir().unwrap();
            let env = make_env(ws.path(), "jq", Some("git"), None);
            apply_auto_status(ws.path(), "git", "jq", Status::Evergreen);
            assert_eq!(load_env_meta(&env).status, Some(Status::Evergreen));
        }

        #[test]
        fn user_set_status_is_never_overwritten() {
            let ws = tempfile::tempdir().unwrap();
            let env = make_env(ws.path(), "jq", Some("git"), Some("jq"));
            seed_status(
                &env,
                Status::Cooked {
                    phase: Some(CookedPhase::Waiting),
                    detail: None,
                },
                StatusSource::User,
            );
            apply_auto_status(ws.path(), "git", "jq", Status::Evergreen);
            assert!(matches!(
                load_env_meta(&env).status,
                Some(Status::Cooked {
                    phase: Some(CookedPhase::Waiting),
                    ..
                })
            ));
        }

        #[test]
        fn auto_set_status_can_be_updated_by_auto() {
            let ws = tempfile::tempdir().unwrap();
            let env = make_env(ws.path(), "p", Some("github"), Some("p"));
            seed_status(
                &env,
                Status::Cooked {
                    phase: None,
                    detail: None,
                },
                StatusSource::Auto {
                    cookbook: Some("cook".to_string()),
                },
            );
            apply_auto_status(ws.path(), "github", "p", done());
            assert_eq!(load_env_meta(&env).status, Some(done()));
        }

        #[test]
        fn ownership_mismatch_is_ignored() {
            let ws = tempfile::tempdir().unwrap();
            let env = make_env(ws.path(), "p", Some("github"), Some("p"));
            apply_auto_status(ws.path(), "git", "p", Status::Evergreen);
            assert_eq!(load_env_meta(&env).status, None);
        }

        #[test]
        fn recipe_mismatch_is_ignored() {
            let ws = tempfile::tempdir().unwrap();
            let env = make_env(ws.path(), "jq", Some("git"), Some("other"));
            apply_auto_status(ws.path(), "git", "jq", Status::Evergreen);
            assert_eq!(load_env_meta(&env).status, None);
        }

        #[test]
        fn non_cookbook_settable_status_is_ignored() {
            let ws = tempfile::tempdir().unwrap();
            let env = make_env(ws.path(), "jq", Some("git"), Some("jq"));
            apply_auto_status(
                ws.path(),
                "git",
                "jq",
                Status::Cooked {
                    phase: Some(CookedPhase::Active),
                    detail: Some(StatusDetail {
                        source: "x".into(),
                        label: "active".into(),
                        info: None,
                    }),
                },
            );
            assert_eq!(load_env_meta(&env).status, None);
        }

        #[test]
        fn unchanged_status_appends_no_event() {
            let ws = tempfile::tempdir().unwrap();
            let env = make_env(ws.path(), "jq", Some("git"), Some("jq"));
            apply_auto_status(ws.path(), "git", "jq", Status::Evergreen);
            let n = load_env_meta(&env).event_log.len();
            apply_auto_status(ws.path(), "git", "jq", Status::Evergreen);
            assert_eq!(load_env_meta(&env).event_log.len(), n, "no-op must not log");
        }

        #[test]
        fn missing_env_dir_is_a_noop() {
            let ws = tempfile::tempdir().unwrap();
            // No env created; must not panic or create anything.
            apply_auto_status(ws.path(), "git", "ghost", Status::Evergreen);
            assert!(!ws.path().join("ghost").exists());
        }
    }

    // P1: the override invariant the army review flagged as B1's acceptance gate.
    // For any random sequence of auto marks applied after a user mark, the
    // user's status must survive untouched.
    mod override_invariant {
        use super::super::apply_auto_status;
        use crate::meta::{
            DoneOutcome, EnvStats, EventLogEntry, EventType, Status, load_env_meta, now_utc,
            save_env_meta,
        };
        use proptest::prelude::*;

        fn settable_status() -> impl Strategy<Value = Status> {
            prop_oneof![
                Just(Status::Evergreen),
                Just(Status::Done { outcome: None }),
                Just(Status::Done {
                    outcome: Some(DoneOutcome::Completed)
                }),
                Just(Status::Done {
                    outcome: Some(DoneOutcome::Abandoned)
                }),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]
            #[test]
            fn user_mark_survives_any_auto_sequence(
                user_status in settable_status(),
                autos in prop::collection::vec(("[a-z]{1,5}", settable_status()), 0..12),
            ) {
                let ws = tempfile::tempdir().unwrap();
                let env_dir = ws.path().join("env");
                std::fs::create_dir_all(&env_dir).unwrap();
                // A user mark: status + StatusChange event with set_by="user"
                // (this is exactly what env_mark writes for MarkSource::User).
                let now = now_utc();
                let meta = EnvStats {
                    cookbook: Some("git".to_string()),
                    recipe: Some("env".to_string()),
                    status: Some(user_status.clone()),
                    event_log: vec![EventLogEntry {
                        event_type: EventType::StatusChange,
                        detail: "user".to_string(),
                        set_by: Some(crate::meta::StatusSource::User),
                        started: now,
                        ended: Some(now),
                    }],
                    ..Default::default()
                };
                save_env_meta(&env_dir, &meta).unwrap();

                for (cookbook, status) in autos {
                    apply_auto_status(ws.path(), &cookbook, "env", status);
                }

                prop_assert_eq!(load_env_meta(&env_dir).status, Some(user_status));
            }
        }
    }
}
