use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use enwiro_sdk::rpc::EnwiroRpcClient;
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const CLIENT_NAME: &str = "aw-watcher-enwiro";
const EVENT_TYPE: &str = "currentenv";
const DEFAULT_AW_BASE_URL: &str = "http://localhost:5600";

fn aw_base_url() -> String {
    std::env::var("AW_BASE_URL").unwrap_or_else(|_| DEFAULT_AW_BASE_URL.to_string())
}
const INTERVAL: Duration = Duration::from_secs(5);
const PULSETIME_SECS: f64 = 15.0;
const META_CACHE_TTL: Duration = Duration::from_secs(10);

fn is_known_env(env_name: &str) -> bool {
    enwiro_envs_dir().join(env_name).is_dir()
}

fn enwiro_envs_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ENWIRO_ENVS_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".enwiro_envs");
    }
    PathBuf::from(".enwiro_envs")
}

fn load_env_metadata(env_name: &str) -> Map<String, Value> {
    let mut out = Map::new();
    let base = enwiro_envs_dir().join(env_name);

    if let Ok(text) = std::fs::read_to_string(base.join("meta.json"))
        && let Ok(Value::Object(meta)) = serde_json::from_str::<Value>(&text)
    {
        for key in ["description", "cookbook"] {
            if let Some(Value::String(v)) = meta.get(key)
                && !v.is_empty()
            {
                out.insert(key.to_string(), Value::String(v.clone()));
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(base.join("gear.d")) {
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
            .collect();
        paths.sort();
        for path in paths {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let Some(Value::Object(gear)) = value.get("gear") else {
                continue;
            };
            let source = gear_source_from_stem(path.file_stem().and_then(|s| s.to_str()));
            for (gear_name, gear_def) in gear {
                if let Some(url) = gear_def
                    .get("web")
                    .and_then(|w| w.get("page"))
                    .and_then(|p| p.get("url"))
                    .and_then(|u| u.as_str())
                {
                    let key = format!("{source}-{gear_name}-url");
                    out.insert(key, Value::String(url.to_string()));
                }
            }
        }
    }
    out
}

/// `${ENWIRO_ENVS_DIR}/<env>/gear.d/<stem>.json` -> short source name.
/// Strips the `cookbook-` / `garnish-` / `bridge-` plugin-kind prefix so
/// the flattened key reads `github-issue-url`, not `cookbook-github-issue-url`.
fn gear_source_from_stem(stem: Option<&str>) -> String {
    let Some(stem) = stem else {
        return "unknown".to_string();
    };
    for prefix in ["cookbook-", "garnish-", "bridge-"] {
        if let Some(rest) = stem.strip_prefix(prefix)
            && !rest.is_empty()
        {
            return rest.to_string();
        }
    }
    stem.to_string()
}

struct MetaCache {
    entries: HashMap<String, (Instant, Map<String, Value>)>,
}

impl MetaCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn get(&mut self, env_name: &str) -> Map<String, Value> {
        let now = Instant::now();
        if let Some((loaded_at, data)) = self.entries.get(env_name)
            && now.duration_since(*loaded_at) < META_CACHE_TTL
        {
            return data.clone();
        }
        let data = load_env_metadata(env_name);
        self.entries
            .insert(env_name.to_string(), (now, data.clone()));
        data
    }
}

fn build_heartbeat_data(env_name: &str, cache: &mut MetaCache) -> Value {
    let mut obj = Map::new();
    obj.insert("env".to_string(), Value::String(env_name.to_string()));
    obj.insert("title".to_string(), Value::String(env_name.to_string()));
    for (k, v) in cache.get(env_name) {
        obj.insert(k, v);
    }
    Value::Object(obj)
}

struct AwClient {
    base_url: String,
}

impl AwClient {
    fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    fn create_bucket(&self, bucket_id: &str, hostname: &str) -> Result<()> {
        let url = format!("{}/api/0/buckets/{}", self.base_url, bucket_id);
        let body = serde_json::json!({
            "client": CLIENT_NAME,
            "type": EVENT_TYPE,
            "hostname": hostname,
        });
        ureq::post(&url)
            .send_json(body)
            .map_err(anyhow::Error::new)?;
        Ok(())
    }

    fn heartbeat(&self, bucket_id: &str, data: &Value, pulsetime: f64) -> Result<()> {
        let url = format!(
            "{}/api/0/buckets/{}/heartbeat?pulsetime={}",
            self.base_url, bucket_id, pulsetime
        );
        let timestamp = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .context("format timestamp")?;
        let body = serde_json::json!({
            "timestamp": timestamp,
            "duration": 0,
            "data": data,
        });
        ureq::post(&url)
            .send_json(body)
            .map_err(anyhow::Error::new)?;
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Explicit INFO default: with the `env-filter` feature unified in via
    // enwiro-sdk, plain `fmt::init()` logs nothing unless RUST_LOG is set,
    // which would leave the daemon's bridge log channel empty. No ANSI:
    // stdout is read by the daemon, not a terminal.
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("metadata") => {
            let metadata = enwiro_sdk::bridge::BridgeMetadata::with_capabilities([
                enwiro_sdk::bridge::BridgeCapability::Listen,
            ]);
            println!("{}", metadata.to_json());
            Ok(())
        }
        Some("listen") => listen().await,
        _ => anyhow::bail!("usage: enwiro-bridge-activitywatch <listen|metadata>"),
    }
}

/// Long-running watch loop, spawned and supervised by enwiro-daemon
/// (issue #485). Polls the daemon's `env.current` RPC and heartbeats the
/// active env to aw-server, so it works with any adapter that emits
/// switch events (issue #710).
async fn listen() -> Result<()> {
    let hostname_os = hostname::get().context("read hostname")?;
    let hostname = hostname_os.to_string_lossy().into_owned();
    let bucket_id = format!("{}_{}", CLIENT_NAME, hostname);
    let base_url = aw_base_url();
    let aw = AwClient::new(&base_url);

    loop {
        match aw.create_bucket(&bucket_id, &hostname) {
            Ok(()) => break,
            Err(e) => {
                tracing::warn!(error = %e, "waiting for aw-server at {base_url}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    tracing::info!(%bucket_id, %base_url, "watching the daemon's active env");

    let mut cache = MetaCache::new();
    let mut client = None;

    loop {
        if client.is_none() {
            match enwiro_sdk::rpc::connect().await {
                Ok(c) => client = Some(c),
                Err(e) => tracing::warn!(error = %e, "waiting for enwiro-daemon RPC socket"),
            }
        }
        if let Some(c) = &client {
            match EnwiroRpcClient::env_current(c).await {
                Ok(result) => match result.env_name {
                    Some(env_name) if is_known_env(&env_name) => {
                        let data = build_heartbeat_data(&env_name, &mut cache);
                        if let Err(e) = aw.heartbeat(&bucket_id, &data, PULSETIME_SECS) {
                            tracing::warn!(error = %e, "heartbeat failed");
                        }
                    }
                    Some(env_name) => {
                        tracing::debug!(
                            %env_name,
                            "daemon reports an active env but its dir is missing - skipping",
                        );
                    }
                    None => {
                        tracing::debug!("no switch event observed since daemon start - skipping");
                    }
                },
                Err(e) => {
                    // The jsonrpsee client is dead once its connection drops
                    // (e.g. daemon restart); rebuild it on the next tick.
                    tracing::warn!(error = %e, "env.current failed - reconnecting");
                    client = None;
                }
            }
        }
        tokio::time::sleep(INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_data_sets_title_to_env_name() {
        let mut cache = MetaCache::new();
        let data = build_heartbeat_data("chezmoi", &mut cache);
        assert_eq!(data["title"], serde_json::json!("chezmoi"));
    }

    #[test]
    fn gear_source_strips_kind_prefix() {
        assert_eq!(gear_source_from_stem(Some("cookbook-github")), "github");
        assert_eq!(gear_source_from_stem(Some("garnish-just")), "just");
        assert_eq!(gear_source_from_stem(Some("bridge-rofi")), "rofi");
    }

    #[test]
    fn gear_source_preserves_unprefixed_stem() {
        assert_eq!(gear_source_from_stem(Some("vehicle")), "vehicle");
        assert_eq!(gear_source_from_stem(Some("cookbook-")), "cookbook-");
        assert_eq!(gear_source_from_stem(None), "unknown");
    }
}
