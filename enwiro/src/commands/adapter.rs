use anyhow::{Context, bail};
use std::process::Command;

pub trait EnwiroAdapterTrait {
    fn get_active_environment_name(&self) -> anyhow::Result<String>;
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
}
impl EnwiroAdapterExternal {
    pub fn new(adapter_name: &str) -> Self {
        Self {
            adapter_command: format!("enwiro-adapter-{}", adapter_name),
        }
    }
}

pub struct EnwiroAdapterNone {}

impl EnwiroAdapterTrait for EnwiroAdapterNone {
    fn get_active_environment_name(&self) -> anyhow::Result<String> {
        bail!("Could not determine active environment because no adapter is configured.")
    }
}
