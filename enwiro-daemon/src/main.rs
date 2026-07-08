use std::path::PathBuf;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let _guard = enwiro_sdk::init_logging("enwiro-daemon.log");
    let config: enwiro_daemon::ConfigurationValues = enwiro_sdk::config::load_user_config("enwiro")
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    enwiro_daemon::run(
        enwiro_daemon::DaemonConfig {
            workspaces_directory: PathBuf::from(config.workspaces_directory),
            container_runtime: config.container_runtime,
            adapter: config.adapter,
        },
        enwiro_daemon::meta::record_switch_per_env,
    )
    .await
}
