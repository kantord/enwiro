use anyhow::{Context, bail};
use std::io::Write;
use std::process::{Command, Stdio};

use crate::plugin::{PluginKind, get_plugins};

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ManagedEnvInfo {
    pub name: String,
    pub slot_score: f64,
}

pub trait EnwiroAdapterTrait {
    fn get_active_environment_name(&self) -> anyhow::Result<String>;
    fn activate(&self, name: &str, managed_envs: &[ManagedEnvInfo]) -> anyhow::Result<()>;
}

pub struct EnwiroAdapterExternal {
    adapter_command: String,
}

impl EnwiroAdapterTrait for EnwiroAdapterExternal {
    fn get_active_environment_name(&self) -> anyhow::Result<String> {
        tracing::debug!("Querying adapter for active environment");
        let output = Command::new(&self.adapter_command)
            .arg("get-active-workspace-id")
            .output()
            .context("Adapter failed to determine active environment name")?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            Ok(stdout.trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Error: {}", stderr);
        }
    }

    fn activate(&self, name: &str, managed_envs: &[ManagedEnvInfo]) -> anyhow::Result<()> {
        tracing::debug!(name = %name, "Activating workspace via adapter");
        let stdin_json =
            serde_json::to_string(managed_envs).context("Could not serialize managed envs")?;

        let mut child = Command::new(&self.adapter_command)
            .arg("activate")
            .arg(name)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("Adapter failed to activate workspace")?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(stdin_json.as_bytes())
                .context("Could not write managed envs to adapter stdin")?;
        }

        let output = child
            .wait_with_output()
            .context("Adapter failed to activate workspace")?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Error: {}", stderr);
        }
    }
}

impl EnwiroAdapterExternal {
    pub fn new(adapter_name: &str) -> anyhow::Result<Self> {
        let plugins = get_plugins(PluginKind::Adapter);
        let plugin = plugins
            .into_iter()
            .find(|p| p.name == adapter_name)
            .context(format!("Adapter '{}' not found", adapter_name))?;

        Ok(Self {
            adapter_command: plugin.executable,
        })
    }
}

pub struct EnwiroAdapterNone {}

impl EnwiroAdapterTrait for EnwiroAdapterNone {
    fn get_active_environment_name(&self) -> anyhow::Result<String> {
        bail!("Could not determine active environment because no adapter is configured.")
    }

    fn activate(&self, _name: &str, _managed_envs: &[ManagedEnvInfo]) -> anyhow::Result<()> {
        bail!("Could not activate workspace because no adapter is configured.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ManagedEnvInfo` must expose a `slot_score` field (not `frecency`),
    /// and that field must serialize to JSON under the key `"slot_score"`.
    #[test]
    fn test_managed_env_info_has_slot_score_field() {
        let info = ManagedEnvInfo {
            name: "my-project".to_string(),
            slot_score: 0.75,
        };
        let json = serde_json::to_string(&info).expect("serialization must succeed");
        let value: serde_json::Value =
            serde_json::from_str(&json).expect("must deserialize back to JSON");

        assert!(
            value.get("slot_score").is_some(),
            "ManagedEnvInfo must serialize `slot_score` as a JSON key, got: {json}"
        );
        assert!(
            value.get("frecency").is_none(),
            "ManagedEnvInfo must NOT serialize a `frecency` key; got: {json}"
        );
        assert!(
            (value["slot_score"].as_f64().unwrap() - 0.75).abs() < 1e-10,
            "slot_score value must round-trip correctly"
        );
    }
}
