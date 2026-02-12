use anyhow::{Context, bail};
use std::process::Command;

use crate::plugin::{PluginKind, get_plugins};

pub trait EnwiroAdapterTrait {
    fn get_active_environment_name(&self) -> anyhow::Result<String>;
    fn activate(&self, name: &str) -> anyhow::Result<()>;
}

pub struct EnwiroAdapterExternal {
    adapter_command: String,
}

impl EnwiroAdapterTrait for EnwiroAdapterExternal {
    fn get_active_environment_name(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.adapter_command)
            .arg("get-active-workspace-id")
            .output()
            .context("Adapter failed to determine active environment name")?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            Ok(stdout.split(':').nth(0).unwrap_or_default().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Error: {}", stderr);
        }
    }

    fn activate(&self, name: &str) -> anyhow::Result<()> {
        let output = Command::new(&self.adapter_command)
            .arg("activate")
            .arg(name)
            .output()
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

    fn activate(&self, _name: &str) -> anyhow::Result<()> {
        bail!("Could not activate workspace because no adapter is configured.")
    }
}
