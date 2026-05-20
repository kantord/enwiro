use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let _guard = enwiro_sdk::init_logging("enwiro-daemon.log");
    let config: enwiro_daemon::ConfigurationValues = enwiro_sdk::config::load_user_config("enwiro")
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    enwiro_daemon::run(
        PathBuf::from(config.workspaces_directory),
        enwiro_daemon::meta::record_switch_per_env,
    )
}
