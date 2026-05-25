use anyhow::Context;
use clap::Parser;
use i3ipc_types::event::{Event, Subscribe};
use i3ipc_types::reply::{Node, Workspace};
use tokio_i3ipc::I3;
use tokio_stream::StreamExt;

mod rebalance;

use enwiro_sdk::adapter::{ActivatePayload, ManagedEnvInfo, RunPayload};
use enwiro_sdk::process::ProcessSpec;

#[derive(clap::Args)]
pub struct ListenArgs {
    #[arg(long, default_value_t = 300)]
    pub debounce_secs: u64,
}

#[derive(Parser)]
enum EnwiroAdapterI3WmCLI {
    GetActiveWorkspaceId(GetActiveWorkspaceIdArgs),
    Activate(ActivateArgs),
    Listen(ListenArgs),
    Run(RunArgs),
}

#[derive(clap::Args)]
pub struct GetActiveWorkspaceIdArgs {}

#[derive(clap::Args)]
pub struct ActivateArgs {
    pub name: String,
}

#[derive(clap::Args)]
pub struct RunArgs {}

/// Best-effort parse; activate falls back to defaults so a malformed
/// payload doesn't block the workspace switch.
fn read_activate_payload() -> ActivatePayload {
    use std::io::Read;
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return ActivatePayload::default();
    }
    serde_json::from_str(&buf).unwrap_or_default()
}

fn build_run_shell_argv(command: &str, args: &[String]) -> String {
    std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Child is detached intentionally — enw exits before the terminal does.
fn spawn_run_in_terminal(payload: &RunPayload) -> anyhow::Result<()> {
    let quoted_argv = build_run_shell_argv(&payload.command, &payload.args);
    ProcessSpec::new("i3-sensible-terminal")
        .arg("-e")
        .arg("sh")
        .arg("-c")
        .arg(&quoted_argv)
        .into_command_in_env(&payload.env_name, std::path::Path::new(&payload.env_path))
        .spawn()
        .with_context(|| format!("Failed to spawn terminal for `{}`", payload.command))?;
    Ok(())
}

/// Default open command used to spawn one window per gear web URL.
/// `chromium --app=<url>` opens a chromeless window so each URL reads
/// visually as a standalone app, and creates a fresh window per
/// invocation (no window-reuse problem on subsequent activations).
fn default_web_open_command() -> Vec<String> {
    vec!["chromium".to_string(), "--app={url}".to_string()]
}

/// Schema for the i3 adapter's user-level TOML at
/// `~/.config/enwiro/adapter-i3wm.toml`. Every field is optional so
/// missing or partially-populated configs fall through to defaults
/// rather than failing to parse.
#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct AdapterConfig {
    web_open_command: Option<Vec<String>>,
}

/// Falls back to `default_web_open_command` when the file is missing,
/// malformed, or has no `web_open_command` field.
fn load_web_open_command(config_path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(config_path)
        .ok()
        .and_then(|s| toml::from_str::<AdapterConfig>(&s).ok())
        .and_then(|c| c.web_open_command)
        .unwrap_or_else(default_web_open_command)
}

/// Resolve the adapter's config path (`~/.config/enwiro/adapter-i3wm.toml`
/// on Linux). Returns `None` if the user's home directory can't be
/// determined - caller falls back to defaults.
fn adapter_config_path() -> Option<std::path::PathBuf> {
    enwiro_sdk::config::user_config_path("adapter-i3wm").ok()
}

/// Walk a merged gear JSON object and collect every `gear.<name>.web.<entry>.url`.
fn collect_web_urls(gear: &serde_json::Value) -> Vec<String> {
    let Some(obj) = gear.as_object() else {
        return Vec::new();
    };
    let mut urls = Vec::new();
    for gear_value in obj.values() {
        let Some(web) = gear_value.get("web").and_then(|v| v.as_object()) else {
            continue;
        };
        for entry in web.values() {
            if let Some(url) = entry.get("url").and_then(|v| v.as_str()) {
                urls.push(url.to_string());
            }
        }
    }
    urls
}

/// Walk a merged gear JSON object and collect every
/// `gear.<name>.linux-gui.<entry>.command` as an argv vector. Empty argvs and
/// non-string elements are filtered out so callers can assume each returned
/// vector has at least a binary at index 0 and is fully UTF-8.
fn collect_gui_commands(gear: &serde_json::Value) -> Vec<Vec<String>> {
    let Some(obj) = gear.as_object() else {
        return Vec::new();
    };
    let mut commands = Vec::new();
    for gear_value in obj.values() {
        let Some(gui) = gear_value.get("linux-gui").and_then(|v| v.as_object()) else {
            continue;
        };
        for entry in gui.values() {
            let Some(arr) = entry.get("command").and_then(|v| v.as_array()) else {
                continue;
            };
            let argv: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if argv.is_empty() {
                continue;
            }
            commands.push(argv);
        }
    }
    commands
}

/// POSIX-shell-quote a single argument: leave it bare if every character is in
/// a conservative safe set, otherwise wrap in single quotes (escaping any
/// internal `'`). i3 invokes `exec`'s payload via `sh -c`, so every component
/// of the constructed command line must be quoted before joining.
fn shell_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '-' | '.' | '/' | '=' | ':' | '@' | ',' | '+' | '%')
        });
    if safe {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// Resolve the absolute path of the `enw` binary. Looks for a sibling of the
/// running adapter binary (the install layout that `cargo install` and
/// `just install-dev` both produce). Falls back to the bare name `enw` when
/// the current-exe lookup fails - only useful if i3's `PATH` happens to
/// include the install dir.
///
/// Required because `i3-msg exec` runs the payload via i3's own session
/// `PATH`, which on a typical setup does not include `~/.cargo/bin`. With a
/// bare `enw`, `sh -c "enw …"` fails with exit 127, and i3's IPC has already
/// returned success by then - the failure is silent.
fn resolve_enw_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("enw")))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "enw".to_string())
}

/// Wraps the user's `web_open_command` inside
/// `<absolute-enw> wrap <command> <env> -- <args>` so the spawned process
/// inherits `cwd` / `ENWIRO_ENV` from `enw wrap`, while `i3-msg exec`
/// supplies daemonization and graphical-session env normalization. The
/// absolute `enw` path is required — see [`resolve_enw_binary`].
struct WrapPayloadBuilder<'a> {
    enw_binary: String,
    env_name: &'a str,
    command_template: &'a [String],
}

impl<'a> WrapPayloadBuilder<'a> {
    fn new(env_name: &'a str, command_template: &'a [String]) -> Self {
        Self {
            enw_binary: resolve_enw_binary(),
            env_name,
            command_template,
        }
    }

    /// Substitutes `{url}` into every argv element and shell-quotes them.
    /// `None` only when `command_template` is empty — callers guard upstream.
    fn payload_for(&self, url: &str) -> Option<String> {
        let (command_name, child_args) = self.command_template.split_first()?;
        let mut parts: Vec<String> = vec![
            self.enw_binary.clone(),
            "wrap".into(),
            command_name.replace("{url}", url),
            self.env_name.to_string(),
            "--".into(),
        ];
        parts.extend(child_args.iter().map(|a| a.replace("{url}", url)));
        let inner = parts
            .iter()
            .map(|p| shell_quote(p))
            .collect::<Vec<_>>()
            .join(" ");
        Some(format!("exec {inner}"))
    }
}

/// Spawn the configured open command for every URL collected from gear, via
/// `i3-msg exec -- <enw> wrap <command> <env> -- <args>`. Best-effort:
/// failures are logged and do not interrupt activation.
fn open_gear_urls(gear: &serde_json::Value, command_template: &[String], env_name: &str) {
    let urls = collect_web_urls(gear);
    if urls.is_empty() {
        return;
    }
    if command_template.is_empty() {
        tracing::warn!(skipped = urls.len(), "web_open_command is empty");
        return;
    }
    let builder = WrapPayloadBuilder::new(env_name, command_template);
    for url in urls {
        let Some(payload) = builder.payload_for(&url) else {
            continue;
        };
        match std::process::Command::new("i3-msg").arg(&payload).spawn() {
            Ok(_) => tracing::info!(payload = %payload, "Spawned open command via i3-msg exec"),
            Err(e) => {
                tracing::warn!(error = %e, payload = %payload, "Failed to invoke i3-msg exec");
            }
        }
    }
}

/// `None` only when `argv` is empty (collectors already filter; belt-and-suspenders).
fn build_gui_payload(enw_binary: &str, env_name: &str, argv: &[String]) -> Option<String> {
    let (binary, args) = argv.split_first()?;
    let mut parts: Vec<String> = vec![
        enw_binary.to_string(),
        "wrap".into(),
        binary.clone(),
        env_name.to_string(),
        "--".into(),
    ];
    parts.extend(args.iter().cloned());
    let inner = parts
        .iter()
        .map(|p| shell_quote(p))
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("exec {inner}"))
}

/// Spawn every linux-gui command collected from gear, via
/// `i3-msg exec -- <enw> wrap <argv> <env>`. Each command's binary is
/// PATH-resolved via `which::which` first; missing binaries are logged and
/// skipped so partially-installed setups (e.g. obsidian present, zotero
/// absent) still activate cleanly. Best-effort like `open_gear_urls`:
/// activation never fails on auto-open issues.
///
/// Cookbooks ALSO check `which` at gear-emit time (see e.g.
/// `enwiro-cookbook-obsidian::cmd_gear`) to keep their gear files clean.
/// This adapter-side check is the safety net for stale gear files - a
/// binary that was installed at emit time but removed before activation.
/// Both layers are intentional, not redundant.
fn spawn_gui_commands(gear: &serde_json::Value, env_name: &str) {
    let commands = collect_gui_commands(gear);
    if commands.is_empty() {
        return;
    }
    let enw_binary = resolve_enw_binary();
    for argv in commands {
        if let Err(e) = which::which(&argv[0]) {
            tracing::warn!(
                binary = %argv[0],
                error = %e,
                "Skipping linux-gui spawn: binary not found on PATH",
            );
            continue;
        }
        let Some(payload) = build_gui_payload(&enw_binary, env_name, &argv) else {
            continue;
        };
        match std::process::Command::new("i3-msg").arg(&payload).spawn() {
            Ok(_) => {
                tracing::info!(payload = %payload, "Spawned linux-gui command via i3-msg exec")
            }
            Err(e) => {
                tracing::warn!(error = %e, payload = %payload, "Failed to invoke i3-msg exec");
            }
        }
    }
}

/// Parse newline-delimited JSON from `enwiro ls --json`.
///
/// Each line is parsed independently as a `serde_json::Value`. Lines that are
/// blank or fail to parse are silently skipped. Only entries where
/// `cookbook == "_"` AND `scores.slot` is a number are included in the output.
fn parse_managed_envs(json_lines: &str) -> Vec<ManagedEnvInfo> {
    json_lines
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            // Only include entries managed by enwiro itself (cookbook == "_").
            if value.get("cookbook")?.as_str()? != "_" {
                return None;
            }
            let name = value.get("name")?.as_str()?.to_string();
            let slot_score = value.get("scores")?.get("slot")?.as_f64()?;
            Some(ManagedEnvInfo { name, slot_score })
        })
        .collect()
}

/// Run `enwiro ls --json`, capture stdout, and parse via `parse_managed_envs`.
///
/// Returns an empty vec on any subprocess or parse error.
fn fetch_managed_envs() -> Vec<ManagedEnvInfo> {
    let enwiro_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("enw")))
        .unwrap_or_else(|| std::path::PathBuf::from("enw"));
    let output = std::process::Command::new(&enwiro_bin)
        .args(["ls", "--json"])
        .output();
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !out.status.success() {
                tracing::warn!(status = %out.status, stderr = %stderr, "enwiro ls failed");
            } else if stdout.is_empty() {
                tracing::warn!("enwiro ls returned empty output");
            } else {
                tracing::debug!(lines = stdout.lines().count(), "enwiro ls output");
            }
            parse_managed_envs(&stdout)
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to run enwiro ls --json");
            vec![]
        }
    }
}

async fn run_i3_command(i3: &mut I3, command: &str) -> anyhow::Result<()> {
    let outcomes = i3.run_command(command).await?;
    if let Some(outcome) = outcomes.first()
        && !outcome.success
    {
        let msg = outcome.error.as_deref().unwrap_or("unknown error");
        tracing::error!(error = %msg, "i3 command failed");
        anyhow::bail!("i3 command failed: {}", msg);
    }
    Ok(())
}

async fn apply_plan(i3: &mut I3, plan: &rebalance::plan::Plan) -> anyhow::Result<()> {
    use rebalance::compile::compile;
    use rebalance::i3_op::render;
    for op in compile(plan) {
        tracing::info!(op = ?op, "i3 op");
        run_i3_command(i3, &render(&op)).await?;
    }
    Ok(())
}

/// Translate i3's workspace list + the managed-env score table into typed
/// inputs for the rebalance pipeline.
///
/// Returns `(managed_envs, unmanaged_slots)`. An env is "managed" iff it has
/// an entry in `managed_envs` (i.e. enwiro knows about it). Workspaces whose
/// name doesn't parse as `N: env` extract to env_name="" — these are always
/// unmanaged.
fn snapshot_for_rebalance(
    workspaces: &[Workspace],
    managed_envs: &[ManagedEnvInfo],
) -> (Vec<rebalance::types::Env>, Vec<rebalance::types::Slot>) {
    use rebalance::types::{Env, EnvName, Slot};
    let score_map: std::collections::HashMap<&str, f64> = managed_envs
        .iter()
        .map(|e| (e.name.as_str(), e.slot_score))
        .collect();
    let mut managed = Vec::new();
    let mut unmanaged = Vec::new();
    for ws in workspaces {
        let env_name = extract_environment_name(ws);
        match score_map.get(env_name.as_str()) {
            Some(&score) => managed.push(Env {
                name: EnvName(env_name),
                slot: Slot(ws.num),
                score,
            }),
            None => unmanaged.push(Slot(ws.num)),
        }
    }
    (managed, unmanaged)
}

/// Lowest workspace number not currently occupied by ANY workspace (managed
/// or unmanaged). Used as the initial slot for a freshly-activating env.
fn lowest_unused_slot(workspaces: &[Workspace]) -> rebalance::types::Slot {
    let used: std::collections::HashSet<i32> = workspaces.iter().map(|ws| ws.num).collect();
    let mut n = 1;
    while used.contains(&n) {
        n += 1;
    }
    rebalance::types::Slot(n)
}

fn current_slot_map(
    managed: &[rebalance::types::Env],
) -> std::collections::HashMap<rebalance::types::EnvName, rebalance::types::Slot> {
    managed.iter().map(|e| (e.name.clone(), e.slot)).collect()
}

/// Rate limit: true iff no prior rebalance has run, or `debounce` has elapsed
/// since the last one. `now` is injected for testability.
fn should_rebalance(
    last: Option<std::time::Instant>,
    debounce: std::time::Duration,
    now: std::time::Instant,
) -> bool {
    match last {
        None => true,
        Some(last_instant) => now.duration_since(last_instant) >= debounce,
    }
}

fn workspace_is_empty(tree: &Node, ws_name: &str) -> bool {
    fn find_workspace<'a>(node: &'a Node, name: &str) -> Option<&'a Node> {
        if node.node_type == i3ipc_types::reply::NodeType::Workspace
            && node.name.as_deref() == Some(name)
        {
            return Some(node);
        }
        for child in &node.nodes {
            if let Some(found) = find_workspace(child, name) {
                return Some(found);
            }
        }
        None
    }
    match find_workspace(tree, ws_name) {
        Some(ws) => ws.nodes.is_empty() && ws.floating_nodes.is_empty(),
        None => true,
    }
}

fn extract_environment_name(workspace: &Workspace) -> String {
    workspace
        .name
        .split_once(':')
        .map(|(_, name)| name.trim().to_string())
        .unwrap_or_default()
}

fn extract_environment_name_from_str(name: &str) -> String {
    name.split_once(':')
        .map(|(_, rest)| rest.trim().to_string())
        .unwrap_or_else(|| name.to_string())
}

fn format_workspace_switch_event(env_name: &str, timestamp: i64) -> String {
    serde_json::json!({
        "type": "workspace_switch",
        "timestamp": timestamp,
        "env_name": env_name,
    })
    .to_string()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let _guard = enwiro_sdk::init_logging("enwiro-adapter-i3wm.log");

    let args = EnwiroAdapterI3WmCLI::parse();

    match args {
        EnwiroAdapterI3WmCLI::GetActiveWorkspaceId(_) => {
            let mut i3 = I3::connect().await?;
            let workspaces = i3.get_workspaces().await?;
            tracing::debug!(count = workspaces.len(), "Retrieved workspaces");
            let focused_workspace = workspaces
                .into_iter()
                .find(|workspace| workspace.focused)
                .context("No active workspace. This should never happen.")?;
            let environment_name = extract_environment_name(&focused_workspace);
            tracing::debug!(name = %environment_name, "Extracted environment name");
            print!("{}", environment_name);
        }
        EnwiroAdapterI3WmCLI::Activate(args) => {
            use rebalance::derive::derive;
            use rebalance::i3_op::{I3Op, render};
            use rebalance::optimize::optimize;
            use rebalance::types::{Env, EnvName, Handle, Slot};

            let payload = read_activate_payload();
            let mut i3 = I3::connect().await?;
            let workspaces = i3.get_workspaces().await?;
            tracing::debug!(count = workspaces.len(), name = %args.name, "Activating environment");

            if let Some(existing) = workspaces
                .iter()
                .find(|ws| extract_environment_name(ws) == args.name)
            {
                // Re-activation: gear stays silent (single-instance apps would
                // yank focus; chromium app-mode would multiply windows).
                tracing::info!(workspace = %existing.name, "Found existing workspace");
                let op = I3Op::Focus {
                    ws: Handle(existing.name.clone()),
                };
                run_i3_command(&mut i3, &render(&op)).await?;
            } else {
                let (mut existing, mut unmanaged) =
                    snapshot_for_rebalance(&workspaces, &payload.managed_envs);

                // Exclude the focused empty workspace: i3 reaps empty
                // workspaces when focus moves away, so the spawn Focus at
                // the end of compile will destroy it. Including it would
                // produce a plan that references a workspace that no
                // longer exists after the Focus fires.
                if let Some(focused_ws) = workspaces.iter().find(|ws| ws.focused) {
                    let focused_env = extract_environment_name(focused_ws);
                    if let Some(pos) = existing.iter().position(|e| e.name.0 == focused_env) {
                        let tree = i3.get_tree().await?;
                        if workspace_is_empty(&tree, &focused_ws.name) {
                            let removed = existing.remove(pos);
                            unmanaged.push(removed.slot);
                            tracing::debug!(env = %focused_env, slot = removed.slot.0,
                                "Excluding empty focused workspace from rebalance");
                        }
                    }
                }

                let free_num = lowest_unused_slot(&workspaces);
                let score = payload
                    .managed_envs
                    .iter()
                    .find(|e| e.name == args.name)
                    .map(|e| e.slot_score)
                    .unwrap_or(0.0);
                let incoming = Env {
                    name: EnvName(args.name.clone()),
                    slot: free_num,
                    score,
                };
                tracing::info!(env = %args.name, free_num = free_num.0, "Activating new env");
                let spec = optimize(&existing, incoming, &unmanaged, Slot(9));
                let plan = derive(&current_slot_map(&existing), &spec);
                apply_plan(&mut i3, &plan).await?;

                let web_open_command = adapter_config_path()
                    .map(|p| load_web_open_command(&p))
                    .unwrap_or_else(default_web_open_command);
                open_gear_urls(&payload.gear, &web_open_command, &args.name);
                spawn_gui_commands(&payload.gear, &args.name);
            }
        }
        EnwiroAdapterI3WmCLI::Run(_) => {
            let payload = RunPayload::read_from_stdin()?;
            tracing::debug!(env = %payload.env_name, command = %payload.command, "Spawning command in new terminal");
            spawn_run_in_terminal(&payload)?;
        }
        EnwiroAdapterI3WmCLI::Listen(listen_args) => {
            use rebalance::derive::derive;
            use rebalance::optimize::{STABILITY_THRESHOLD, optimize_single_step};
            use rebalance::types::Slot;

            let debounce = std::time::Duration::from_secs(listen_args.debounce_secs);
            let mut last_rebalance: Option<std::time::Instant> = None;
            let mut i3 = I3::connect().await?;
            i3.subscribe([Subscribe::Workspace]).await?;
            let mut listener = i3.listen();
            // Separate command-issuing socket: the listener socket can only
            // stream events.
            let mut i3_cmd = I3::connect().await?;
            loop {
                let Some(item) = listener.next().await else {
                    tracing::info!("i3 closed the event stream — exiting Listen loop");
                    break;
                };
                let ws_event = match item {
                    Ok(Event::Workspace(ws_event)) => ws_event,
                    Ok(_) => continue,
                    Err(e) => {
                        tracing::error!(error = %e, "i3 IPC error on listener stream — exiting Listen loop");
                        break;
                    }
                };
                let Some(current) = ws_event.current else {
                    continue;
                };
                let Some(raw_name) = current.name.filter(|n| !n.is_empty()) else {
                    continue;
                };
                let env_name = extract_environment_name_from_str(&raw_name);
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                println!("{}", format_workspace_switch_event(&env_name, ts));

                let now = std::time::Instant::now();
                if !should_rebalance(last_rebalance, debounce, now) {
                    tracing::debug!("Rebalance skipped (rate limited)");
                    continue;
                }
                tracing::debug!("Rebalance check triggered");
                let managed_envs = fetch_managed_envs();
                tracing::debug!(count = managed_envs.len(), "Fetched managed envs");
                let workspaces = i3_cmd.get_workspaces().await?;
                let (existing, unmanaged) = snapshot_for_rebalance(&workspaces, &managed_envs);
                let spec =
                    optimize_single_step(&existing, &unmanaged, Slot(9), STABILITY_THRESHOLD);
                let plan = derive(&current_slot_map(&existing), &spec);
                if plan.relocations.is_empty() && plan.spawn.is_none() {
                    tracing::debug!("No rebalance needed");
                }
                apply_plan(&mut i3_cmd, &plan).await?;
                last_rebalance = Some(std::time::Instant::now());
            }
        }
    };

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use i3ipc_types::reply::Rect;
    use rstest::rstest;

    fn make_workspace(id: usize, num: i32, name: &str) -> Workspace {
        Workspace {
            id,
            num,
            name: name.to_string(),
            visible: true,
            focused: false,
            urgent: false,
            rect: Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            output: "eDP-1".to_string(),
        }
    }

    #[test]
    fn test_extract_plain_environment_name() {
        let ws = make_workspace(123, 1, "1: my-project");
        assert_eq!(extract_environment_name(&ws), "my-project");
    }

    #[test]
    fn test_extract_empty_for_numbered_workspace() {
        let ws = make_workspace(1, 1, "1");
        assert_eq!(extract_environment_name(&ws), "");
    }

    #[test]
    fn test_extract_name_containing_workspace_number() {
        // Workspace "1: project1" - the name contains "1" as a substring
        let ws = make_workspace(123, 1, "1: project1");
        assert_eq!(extract_environment_name(&ws), "project1");
    }

    #[test]
    fn test_extract_name_containing_workspace_number_in_middle() {
        // Workspace "3: a3b" - the name contains "3" as a substring
        let ws = make_workspace(456, 3, "3: a3b");
        assert_eq!(extract_environment_name(&ws), "a3b");
    }

    /// `ManagedEnvInfo` in the i3wm adapter must deserialize from JSON using the key
    /// `"slot_score"` (not `"frecency"`).  This mirrors the rename on the core side.
    #[test]
    fn test_i3wm_deserializes_slot_score_from_json() {
        let json = r#"[{"name":"my-project","slot_score":0.5}]"#;
        let envs: Vec<ManagedEnvInfo> =
            serde_json::from_str(json).expect("must parse slot_score from JSON");
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].name, "my-project");
        assert!(
            (envs[0].slot_score - 0.5).abs() < 1e-10,
            "slot_score must round-trip from JSON, got {}",
            envs[0].slot_score
        );
    }

    /// Confirm that a JSON payload using the old `"frecency"` key is NOT silently
    /// accepted under the new field name - this guards against accidentally keeping
    /// a serde rename alias.
    #[test]
    fn test_i3wm_does_not_accept_frecency_key() {
        let json = r#"[{"name":"my-project","frecency":0.5}]"#;
        // With `deny_unknown_fields` this would error; without it the struct default (0.0)
        // is used.  Either way, the slot_score must NOT be 0.5 (i.e. the old key is gone).
        let envs: Vec<ManagedEnvInfo> = serde_json::from_str(json).unwrap_or_default();
        if !envs.is_empty() {
            assert!(
                (envs[0].slot_score - 0.5).abs() > 1e-10,
                "slot_score must NOT be populated from the old `frecency` JSON key; \
                 if it is, the rename has a hidden alias"
            );
        }
    }

    // (rename/workspace command builder tests removed — covered by
    // `rebalance::i3_op::tests::*` against the typed `render` function.)

    // ── listen subcommand tests ───────────────────────────────────────────────

    /// Test 1: Output of format_workspace_switch_event is valid JSON and has the
    /// expected keys: `type`, `timestamp`, and `env_name`.
    #[test]
    fn test_format_workspace_switch_event_is_valid_json_with_expected_keys() {
        let output = format_workspace_switch_event("my-project", 1700000000);
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output must be valid JSON");
        assert!(parsed.get("type").is_some(), "missing key: type");
        assert!(parsed.get("timestamp").is_some(), "missing key: timestamp");
        assert!(parsed.get("env_name").is_some(), "missing key: env_name");
    }

    /// Test 2: The `type` field is always the literal string `"workspace_switch"`.
    #[test]
    fn test_format_workspace_switch_event_type_is_workspace_switch() {
        let output = format_workspace_switch_event("any-env", 0);
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output must be valid JSON");
        assert_eq!(
            parsed["type"].as_str(),
            Some("workspace_switch"),
            "type field must be \"workspace_switch\""
        );
    }

    /// Test 3: The `env_name` field is passed through as-is (no transformation).
    #[test]
    fn test_format_workspace_switch_event_env_name_passthrough() {
        let env_name = "some-complex-env_name.with.dots";
        let output = format_workspace_switch_event(env_name, 42);
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output must be valid JSON");
        assert_eq!(
            parsed["env_name"].as_str(),
            Some(env_name),
            "env_name must be passed through unchanged"
        );
    }

    /// Test 4: The `timestamp` field round-trips correctly as an i64.
    #[test]
    fn test_format_workspace_switch_event_timestamp_roundtrips() {
        let timestamp: i64 = 1_700_000_000;
        let output = format_workspace_switch_event("proj", timestamp);
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output must be valid JSON");
        assert_eq!(
            parsed["timestamp"].as_i64(),
            Some(timestamp),
            "timestamp must round-trip as i64"
        );
    }

    /// Test 5: The output does NOT end with a trailing newline - the caller
    /// adds one via `println!`.
    #[test]
    fn test_format_workspace_switch_event_no_trailing_newline() {
        let output = format_workspace_switch_event("proj", 0);
        assert!(
            !output.ends_with('\n'),
            "format_workspace_switch_event must not append a trailing newline; \
             the caller uses println! to add one"
        );
    }

    /// Test 6: The CLI enum has a `Listen` variant that can be constructed.
    /// This test will fail to compile until the variant and `ListenArgs` exist.
    #[test]
    fn test_cli_has_listen_variant() {
        let _ = EnwiroAdapterI3WmCLI::Listen(ListenArgs { debounce_secs: 300 });
    }

    // ── should_rebalance tests ────────────────────────────────────────────────
    //
    // `should_rebalance` is a pure function:
    //
    //   fn should_rebalance(
    //       last: Option<std::time::Instant>,
    //       debounce: std::time::Duration,
    //       now: std::time::Instant,
    //   ) -> bool
    //
    // It answers: "given when the last rebalance ran (if ever), the debounce window,
    // and the current instant, should we run a rebalance now?"
    //
    // The rule:
    //   - If `last` is `None`  → always rebalance (no prior run to suppress it).
    //   - If `now - last < debounce` → skip (too soon).
    //   - If `now - last >= debounce` → rebalance (window has expired, boundary inclusive).

    /// SR-1: No prior rebalance has run - `last` is `None`.
    ///
    /// Expected: `should_rebalance(None, any_debounce, now)` returns `true`.
    #[test]
    fn test_should_rebalance_returns_true_when_no_prior_run() {
        let now = std::time::Instant::now();
        let debounce = std::time::Duration::from_secs(300);

        assert!(
            should_rebalance(None, debounce, now),
            "should_rebalance must return true when last is None (no prior rebalance has run)"
        );
    }

    /// SR-2: A prior rebalance ran, but enough time has elapsed (elapsed > debounce).
    ///
    /// Setup: `last` = 400 seconds ago, `debounce` = 300 s.
    /// elapsed (400 s) > debounce (300 s) → should rebalance.
    ///
    /// Expected: `true`.
    #[test]
    fn test_should_rebalance_returns_true_when_elapsed_exceeds_debounce() {
        let debounce = std::time::Duration::from_secs(300);
        let elapsed_since_last = std::time::Duration::from_secs(400);
        let now = std::time::Instant::now();
        // Simulate `last` by computing a past instant: now - elapsed_since_last
        let last = now
            .checked_sub(elapsed_since_last)
            .expect("test machine clock must support subtracting 400 s from Instant::now()");

        assert!(
            should_rebalance(Some(last), debounce, now),
            "should_rebalance must return true when elapsed ({elapsed_since_last:?}) \
             exceeds debounce ({debounce:?})"
        );
    }

    /// SR-3: A prior rebalance ran recently - elapsed < debounce → skip.
    ///
    /// Setup: `last` = 100 seconds ago, `debounce` = 300 s.
    /// elapsed (100 s) < debounce (300 s) → must skip.
    ///
    /// Expected: `false`.
    #[test]
    fn test_should_rebalance_returns_false_when_elapsed_is_less_than_debounce() {
        let debounce = std::time::Duration::from_secs(300);
        let elapsed_since_last = std::time::Duration::from_secs(100);
        let now = std::time::Instant::now();
        let last = now
            .checked_sub(elapsed_since_last)
            .expect("test machine clock must support subtracting 100 s from Instant::now()");

        assert!(
            !should_rebalance(Some(last), debounce, now),
            "should_rebalance must return false when elapsed ({elapsed_since_last:?}) \
             is less than debounce ({debounce:?})"
        );
    }

    /// SR-4: Boundary - elapsed equals the debounce exactly.
    ///
    /// Setup: `last` = exactly 300 seconds ago, `debounce` = 300 s.
    /// elapsed (300 s) == debounce (300 s) → boundary is inclusive, should rebalance.
    ///
    /// Expected: `true`.
    #[test]
    fn test_should_rebalance_returns_true_when_elapsed_equals_debounce_exactly() {
        let debounce = std::time::Duration::from_secs(300);
        let elapsed_since_last = std::time::Duration::from_secs(300);
        let now = std::time::Instant::now();
        let last = now
            .checked_sub(elapsed_since_last)
            .expect("test machine clock must support subtracting 300 s from Instant::now()");

        assert!(
            should_rebalance(Some(last), debounce, now),
            "should_rebalance must return true when elapsed ({elapsed_since_last:?}) \
             equals debounce exactly ({debounce:?}) - the boundary is inclusive"
        );
    }

    /// The activate stdin payload from core has shape
    /// `{version, managed_envs: [...], gear: {...}}`. The adapter must
    /// deserialize this into `ActivatePayload` and pull `managed_envs` out
    /// for use by the rest of the activate flow.
    #[test]
    fn test_activate_payload_deserializes_versioned_envelope() {
        let json = r#"{
            "version": 1,
            "managed_envs": [{"name": "foo", "slot_score": 0.5}],
            "gear": {
                "pr": {
                    "description": "PR #1",
                    "web": {
                        "page": {
                            "description": "Open the PR",
                            "url": "https://example.com/pr/1"
                        }
                    }
                }
            }
        }"#;

        let parsed: ActivatePayload =
            serde_json::from_str(json).expect("must deserialize new payload shape");

        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.managed_envs.len(), 1);
        assert_eq!(parsed.managed_envs[0].name, "foo");
        assert!(
            parsed.gear.is_object(),
            "gear field must be retained as a JSON object for cycle 7 to consume"
        );
        assert_eq!(
            parsed.gear["pr"]["web"]["page"]["url"],
            "https://example.com/pr/1"
        );
    }

    /// If stdin is empty or malformed (e.g. an old core that sent a bare
    /// array), parsing must yield default values rather than panic - the
    /// adapter should still complete the workspace switch.
    #[test]
    fn test_activate_payload_default_on_invalid_input() {
        let parsed: ActivatePayload = serde_json::from_str("[]").unwrap_or_default();
        assert_eq!(parsed.version, 0);
        assert!(parsed.managed_envs.is_empty());
        assert!(parsed.gear.is_null());
    }

    /// `collect_web_urls` walks `gear.<name>.web.<entry>.url` for every
    /// gear in the merged map and returns each URL.
    #[test]
    fn test_collect_web_urls_gathers_every_url() {
        let gear = serde_json::json!({
            "pr": {
                "description": "PR #1",
                "web": {
                    "page": {"description": "Open PR", "url": "https://example.com/pr/1"}
                }
            },
            "issue": {
                "description": "Issue #2",
                "web": {
                    "page": {"description": "Open issue", "url": "https://example.com/issues/2"}
                }
            }
        });

        let mut urls = collect_web_urls(&gear);
        urls.sort();

        assert_eq!(
            urls,
            vec![
                "https://example.com/issues/2".to_string(),
                "https://example.com/pr/1".to_string(),
            ]
        );
    }

    /// Missing or null gear must yield an empty URL list, not a panic.
    #[test]
    fn test_collect_web_urls_handles_null_and_missing_gracefully() {
        assert!(collect_web_urls(&serde_json::Value::Null).is_empty());
        assert!(collect_web_urls(&serde_json::json!({})).is_empty());
        assert!(collect_web_urls(&serde_json::json!({"pr": {"description": "no web"}})).is_empty());
    }

    /// The default open command must use chromium app-mode so each URL
    /// opens in a fresh chromeless window placed in the current i3 workspace.
    #[test]
    fn test_default_web_open_command_is_chromium_app_mode() {
        let cmd = default_web_open_command();
        assert_eq!(cmd[0], "chromium");
        assert!(
            cmd.iter().any(|arg| arg.contains("--app={url}")),
            "default must use chromium --app=<url> for chromeless new windows; got {cmd:?}"
        );
    }

    /// `shell_quote` cases:
    /// - alphanumeric / common-punctuation args stay bare so readable command
    ///   lines stay readable;
    /// - whitespace and shell metacharacters force single-quote wrapping so
    ///   `sh -c` can't reinterpret them;
    /// - internal single quotes are escaped via the POSIX `'\''` idiom
    ///   (close-quote, escaped quote, reopen-quote).
    #[rstest]
    #[case::bare_alphanumeric("chromium", "chromium")]
    #[case::bare_url("--app=https://example.com/path", "--app=https://example.com/path")]
    #[case::wraps_space("hello world", "'hello world'")]
    #[case::wraps_dollar("$HOME", "'$HOME'")]
    #[case::wraps_brace("{url}", "'{url}'")]
    #[case::escapes_single_quote("it's", r"'it'\''s'")]
    fn test_shell_quote_cases(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(shell_quote(input), expected);
    }

    /// `resolve_enw_binary` must produce an absolute path so the spawned
    /// `sh -c` invocation does not depend on i3's PATH. Sibling-of-current-exe
    /// is the install layout used by both `cargo install` and `just install-dev`.
    #[test]
    fn test_resolve_enw_binary_returns_absolute_sibling_path() {
        let resolved = resolve_enw_binary();
        let path = std::path::Path::new(&resolved);
        assert!(
            path.is_absolute(),
            "resolved enw path must be absolute (i3's PATH may not contain the install dir); got {resolved}"
        );
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("enw"),
            "resolved binary name must be `enw`; got {resolved}"
        );
    }

    /// `WrapPayloadBuilder::payload_for` produces the full `exec <enw> wrap
    /// <command> <env> -- <substituted-args>` shape that `enw wrap`'s clap
    /// parser expects, with `{url}` substituted into every argv element and
    /// shell-unsafe characters quoted.
    #[test]
    fn test_payload_for_substitutes_url_and_quotes_args() {
        let template = vec!["chromium".to_string(), "--app={url}".to_string()];
        let builder = WrapPayloadBuilder {
            enw_binary: "enw".to_string(),
            env_name: "my-env",
            command_template: &template,
        };
        let payload = builder
            .payload_for("https://example.com/x")
            .expect("payload_for must yield Some for non-empty template");
        assert_eq!(
            payload,
            "exec enw wrap chromium my-env -- --app=https://example.com/x"
        );
    }

    /// Env names with shell metacharacters are quoted so `sh -c` can't
    /// reinterpret them. Same property exercised more pointedly than via
    /// `shell_quote` alone, since this checks the integration where it
    /// actually matters.
    #[test]
    fn test_payload_for_quotes_env_name_with_spaces() {
        let template = vec!["chromium".to_string(), "--app={url}".to_string()];
        let builder = WrapPayloadBuilder {
            enw_binary: "enw".to_string(),
            env_name: "my env",
            command_template: &template,
        };
        let payload = builder.payload_for("https://example.com").unwrap();
        assert!(
            payload.contains("'my env'"),
            "payload must single-quote env names containing spaces; got: {payload}"
        );
    }

    /// An empty command template yields `None`. Caller is responsible for
    /// guarding upfront; this is the defensive return that keeps a misuse
    /// from producing a malformed `enw wrap` invocation.
    #[test]
    fn test_payload_for_returns_none_for_empty_template() {
        let template: Vec<String> = vec![];
        let builder = WrapPayloadBuilder {
            enw_binary: "enw".to_string(),
            env_name: "env",
            command_template: &template,
        };
        assert!(builder.payload_for("https://example.com").is_none());
    }

    /// Regression: `payload_for` must hand `i3-msg` an absolute `enw`
    /// path, not a bare name. See [`resolve_enw_binary`] for the why.
    /// Asserts the property (absolute path, pointing at `enw`), not the
    /// exact payload string - quoting/format tweaks shouldn't break it.
    #[test]
    fn test_payload_for_uses_absolute_enw_path() {
        let template = vec!["chromium".to_string(), "--app={url}".to_string()];
        let builder = WrapPayloadBuilder::new("my-env", &template);
        let payload = builder.payload_for("https://example.com/x").unwrap();

        // Must be an i3 exec command starting with an absolute path. Bare names
        // (the bug this test guards against) would not start with `/`.
        let after_exec = payload
            .strip_prefix("exec ")
            .unwrap_or_else(|| panic!("payload must begin with 'exec '; got {payload}"));
        let first_token = after_exec.split_whitespace().next().unwrap_or("");
        let first_path = std::path::Path::new(first_token);
        assert!(
            first_path.is_absolute(),
            "first token after `exec` must be an absolute path - i3 spawns via its own \
             session PATH which typically does not include the install dir, so a bare \
             name like `enw` fails silently. Got first token: {first_token}; full payload: {payload}"
        );
        assert_eq!(
            first_path.file_name().and_then(|n| n.to_str()),
            Some("enw"),
            "first token must point at the `enw` binary; got {first_token}"
        );
    }

    /// `collect_gui_commands` walks `gear.<name>.linux-gui.<entry>.command` for
    /// every gear in the merged map and returns each argv as a `Vec<String>`.
    #[test]
    fn test_collect_gui_commands_gathers_every_argv() {
        let gear = serde_json::json!({
            "obsidian": {
                "description": "Obsidian notes",
                "linux-gui": {
                    "app": {"command": ["obsidian"]}
                }
            },
            "zotero": {
                "description": "Zotero references",
                "linux-gui": {
                    "app": {"command": ["zotero", "--no-splash"]}
                }
            }
        });

        let mut commands = collect_gui_commands(&gear);
        commands.sort();

        assert_eq!(
            commands,
            vec![
                vec!["obsidian".to_string()],
                vec!["zotero".to_string(), "--no-splash".to_string()],
            ]
        );
    }

    /// Missing or null gear must yield an empty command list, not a panic.
    /// Mirrors the same property exercised for `collect_web_urls`.
    #[test]
    fn test_collect_gui_commands_handles_null_and_missing_gracefully() {
        assert!(collect_gui_commands(&serde_json::Value::Null).is_empty());
        assert!(collect_gui_commands(&serde_json::json!({})).is_empty());
        assert!(
            collect_gui_commands(&serde_json::json!({
                "pr": {"description": "no linux-gui"}
            }))
            .is_empty()
        );
    }

    /// An empty `command` array is a wire-format mistake (every argv needs
    /// at least a binary). Filter it out at collection time so downstream
    /// callers can `argv[0]` without checking.
    #[test]
    fn test_collect_gui_commands_skips_empty_command_arrays() {
        let gear = serde_json::json!({
            "broken": {
                "description": "Broken entry",
                "linux-gui": {
                    "app": {"command": []}
                }
            }
        });
        assert!(collect_gui_commands(&gear).is_empty());
    }

    /// Non-string elements inside `command` are skipped silently. Schema
    /// validation rejects this at the SDK layer, but the adapter walks the
    /// gear value as raw JSON so it must defend against malformed data on
    /// the floor (e.g. a hand-written gear file that slipped through).
    #[test]
    fn test_collect_gui_commands_filters_non_string_elements() {
        let gear = serde_json::json!({
            "mixed": {
                "description": "Mixed types in command",
                "linux-gui": {
                    "app": {"command": ["obsidian", 42, null, "--flag"]}
                }
            }
        });
        let commands = collect_gui_commands(&gear);
        assert_eq!(
            commands,
            vec![vec!["obsidian".to_string(), "--flag".to_string()]]
        );
    }

    /// `build_gui_payload` produces the full `exec <enw> wrap <argv[0]> <env>
    /// -- <argv[1..]>` shape that `enw wrap`'s clap parser expects, with no
    /// template substitution (the argv IS the command).
    #[test]
    fn test_build_gui_payload_emits_full_invocation() {
        let argv = vec!["obsidian".to_string()];
        let payload = build_gui_payload("enw", "my-env", &argv)
            .expect("build_gui_payload must yield Some for non-empty argv");
        assert_eq!(payload, "exec enw wrap obsidian my-env --");
    }

    /// Args after `argv[0]` are preserved positionally, with `--` separating
    /// them from the `enw wrap` framing arguments.
    #[test]
    fn test_build_gui_payload_preserves_args() {
        let argv = vec!["zotero".to_string(), "--no-splash".to_string()];
        let payload = build_gui_payload("enw", "my-env", &argv).unwrap();
        assert_eq!(payload, "exec enw wrap zotero my-env -- --no-splash");
    }

    /// Env names with shell metacharacters are quoted so `sh -c` can't
    /// reinterpret them. Same property `test_payload_for_quotes_env_name_with_spaces`
    /// exercises for the URL flow, repeated here because the GUI flow
    /// constructs its payload independently.
    #[test]
    fn test_build_gui_payload_quotes_env_name_with_spaces() {
        let argv = vec!["obsidian".to_string()];
        let payload = build_gui_payload("enw", "my env", &argv).unwrap();
        assert!(
            payload.contains("'my env'"),
            "payload must single-quote env names containing spaces; got: {payload}"
        );
    }

    /// An empty argv yields `None`. Collectors filter empty argvs upstream;
    /// this is the defensive return that keeps a hand-rolled caller from
    /// producing a malformed `enw wrap` invocation.
    #[test]
    fn test_build_gui_payload_returns_none_for_empty_argv() {
        let argv: Vec<String> = vec![];
        assert!(build_gui_payload("enw", "env", &argv).is_none());
    }

    /// Regression: `build_gui_payload` must hand `i3-msg` an absolute `enw`
    /// path when constructed via `resolve_enw_binary`. See the URL-flow
    /// counterpart for the why.
    #[test]
    fn test_build_gui_payload_uses_absolute_enw_path() {
        let argv = vec!["obsidian".to_string()];
        let enw_binary = resolve_enw_binary();
        let payload = build_gui_payload(&enw_binary, "my-env", &argv).unwrap();

        let after_exec = payload
            .strip_prefix("exec ")
            .unwrap_or_else(|| panic!("payload must begin with 'exec '; got {payload}"));
        let first_token = after_exec.split_whitespace().next().unwrap_or("");
        let first_path = std::path::Path::new(first_token);
        assert!(
            first_path.is_absolute(),
            "first token after `exec` must be an absolute path. Got first token: {first_token}; full payload: {payload}"
        );
        assert_eq!(
            first_path.file_name().and_then(|n| n.to_str()),
            Some("enw"),
            "first token must point at the `enw` binary; got {first_token}"
        );
    }

    /// `load_web_open_command` must read `web_open_command` from the adapter's
    /// user-level TOML and return it instead of the default when the field is set.
    #[test]
    fn test_load_web_open_command_uses_user_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let toml_path = dir.path().join("adapter-i3wm.toml");
        std::fs::write(
            &toml_path,
            r#"web_open_command = ["firefox", "--new-window", "{url}"]
"#,
        )
        .expect("write toml");

        let cmd = load_web_open_command(&toml_path);
        assert_eq!(
            cmd,
            vec![
                "firefox".to_string(),
                "--new-window".to_string(),
                "{url}".to_string(),
            ],
            "user-configured web_open_command must override the default"
        );
    }

    /// `load_web_open_command` falls back to the default whenever the user
    /// config can't supply a value:
    /// - missing file (dominant production case - most users don't write one);
    /// - malformed TOML (must not crash activation);
    /// - valid TOML without the field (users may configure other fields).
    #[rstest]
    #[case::file_missing(None)]
    #[case::malformed_toml(Some("this is = not [valid toml"))]
    #[case::field_absent(Some("# unrelated comment\n"))]
    fn test_load_web_open_command_falls_back_to_default(#[case] file_contents: Option<&str>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let toml_path = dir.path().join("adapter-i3wm.toml");
        if let Some(contents) = file_contents {
            std::fs::write(&toml_path, contents).expect("write toml");
        }
        assert_eq!(
            load_web_open_command(&toml_path),
            default_web_open_command()
        );
    }

    // ── parse_managed_envs tests ──────────────────────────────────────────────
    //
    // `parse_managed_envs` accepts newline-delimited JSON (one entry per line)
    // produced by `enwiro ls --json` and returns only managed environments
    // (cookbook == "_") that carry a `scores.slot` value.
    //
    // Each returned `ManagedEnvInfo` has:
    //   - `name`       – the `name` field of the JSON entry
    //   - `slot_score` – taken from `scores.slot`

    /// An environment entry with `cookbook == "_"` and a `scores.slot` value is
    /// included in the output.
    #[test]
    fn test_parse_managed_envs_includes_env_with_slot_score() {
        let input = r#"{"cookbook":"_","name":"my-env","sort_order":0,"scores":{"launcher":0.9,"slot":0.7}}"#;
        let result = parse_managed_envs(input);
        assert_eq!(result.len(), 1, "expected one entry, got {}", result.len());
        assert_eq!(result[0].name, "my-env");
        assert!(
            (result[0].slot_score - 0.7).abs() < 1e-10,
            "slot_score must come from scores.slot; got {}",
            result[0].slot_score
        );
    }

    /// A recipe entry (cookbook != "_") is excluded even if it has a scores.slot.
    #[test]
    fn test_parse_managed_envs_excludes_recipe_entry() {
        let input =
            r#"{"cookbook":"github","name":"some-repo","sort_order":0,"scores":{"slot":0.5}}"#;
        let result = parse_managed_envs(input);
        assert!(
            result.is_empty(),
            "recipe entries (cookbook != \"_\") must be excluded; got {:?}",
            result
        );
    }

    /// An environment entry with `cookbook == "_"` but without any `scores` field
    /// is excluded.
    #[test]
    fn test_parse_managed_envs_excludes_env_without_scores() {
        let input = r#"{"cookbook":"_","name":"no-scores-env","sort_order":0}"#;
        let result = parse_managed_envs(input);
        assert!(
            result.is_empty(),
            "entries without a scores field must be excluded; got {:?}",
            result
        );
    }

    /// An environment entry with `cookbook == "_"` and a `scores` object that
    /// lacks the `slot` key is excluded.
    #[test]
    fn test_parse_managed_envs_excludes_env_without_slot_key() {
        let input =
            r#"{"cookbook":"_","name":"no-slot-env","sort_order":0,"scores":{"launcher":0.9}}"#;
        let result = parse_managed_envs(input);
        assert!(
            result.is_empty(),
            "entries whose scores object has no 'slot' key must be excluded; got {:?}",
            result
        );
    }

    /// Empty input string returns an empty vec.
    #[test]
    fn test_parse_managed_envs_empty_input_returns_empty_vec() {
        let result = parse_managed_envs("");
        assert!(result.is_empty(), "empty input must produce an empty vec");
    }

    /// Blank / whitespace-only lines are silently skipped.
    #[test]
    fn test_parse_managed_envs_skips_blank_lines() {
        let input = "\n   \n\t\n";
        let result = parse_managed_envs(input);
        assert!(
            result.is_empty(),
            "blank lines must be skipped silently; got {:?}",
            result
        );
    }

    /// Malformed JSON lines are silently skipped; valid lines on other rows are
    /// still processed.
    #[test]
    fn test_parse_managed_envs_skips_malformed_json_lines() {
        let input = concat!(
            "not-valid-json\n",
            r#"{"cookbook":"_","name":"good-env","sort_order":0,"scores":{"slot":0.3}}"#,
            "\n",
            "{broken",
        );
        let result = parse_managed_envs(input);
        assert_eq!(
            result.len(),
            1,
            "only the valid managed entry must be returned; got {:?}",
            result
        );
        assert_eq!(result[0].name, "good-env");
        assert!((result[0].slot_score - 0.3).abs() < 1e-10);
    }

    /// Multiple valid environment entries all appear in the output, and their
    /// slot_score values are taken from `scores.slot` independently.
    #[test]
    fn test_parse_managed_envs_returns_multiple_entries() {
        let input = concat!(
            r#"{"cookbook":"_","name":"env-a","sort_order":0,"scores":{"slot":0.8}}"#,
            "\n",
            r#"{"cookbook":"_","name":"env-b","sort_order":1,"scores":{"launcher":0.4,"slot":0.2}}"#,
        );
        let result = parse_managed_envs(input);
        assert_eq!(
            result.len(),
            2,
            "both valid entries must be returned; got {:?}",
            result
        );
        let names: Vec<&str> = result.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"env-a"), "env-a must be present");
        assert!(names.contains(&"env-b"), "env-b must be present");
        let a = result.iter().find(|e| e.name == "env-a").unwrap();
        let b = result.iter().find(|e| e.name == "env-b").unwrap();
        assert!(
            (a.slot_score - 0.8).abs() < 1e-10,
            "env-a slot_score wrong: {}",
            a.slot_score
        );
        assert!(
            (b.slot_score - 0.2).abs() < 1e-10,
            "env-b slot_score wrong: {}",
            b.slot_score
        );
    }

    /// Mixed input: recipe, env-without-scores, valid env, malformed - only the
    /// valid env survives.
    #[test]
    fn test_parse_managed_envs_mixed_input_only_valid_env_survives() {
        let input = concat!(
            r#"{"cookbook":"github","name":"a-recipe","sort_order":0}"#,
            "\n",
            r#"{"cookbook":"_","name":"no-scores","sort_order":0}"#,
            "\n",
            r#"{"cookbook":"_","name":"the-keeper","sort_order":0,"scores":{"slot":0.6}}"#,
            "\n",
            "totally-broken",
        );
        let result = parse_managed_envs(input);
        assert_eq!(
            result.len(),
            1,
            "only 'the-keeper' should survive filtering; got {:?}",
            result
        );
        assert_eq!(result[0].name, "the-keeper");
        assert!((result[0].slot_score - 0.6).abs() < 1e-10);
    }
}
