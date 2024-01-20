use std::process::Command;

use crate::config::ConfigurationValues;
use std::io::{Read, Write};

pub trait EnwiroAdapterTrait {
    fn get_active_environment_name(&self) -> String;
}

pub struct EnwiroAdapterExternal {
    adapter_command: String,
}

impl EnwiroAdapterTrait for EnwiroAdapterExternal {
    fn get_active_environment_name(&self) -> String {
        let output = Command::new(&self.adapter_command)
            .arg("get-active-workspace-id")
            .output()
            .expect("Adapter failed to determine active environment name");

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return stdout.to_string();
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            panic!("Error: {}", stderr);
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
    fn get_active_environment_name(&self) -> String {
        panic!("Current environment cannot be automatically determined because no adapter is specified in the configuration.")
    }
}

pub struct CommandContext<R: Read, W: Write> {
    pub config: ConfigurationValues,
    pub reader: R,
    pub writer: W,
    pub adapter: Box<dyn EnwiroAdapterTrait>,
}

impl<R: Read, W: Write> CommandContext<R, W> {
    pub fn new(config: ConfigurationValues, reader: R, writer: W) -> Self {
        let adapter: Box<dyn EnwiroAdapterTrait> = match &config.adapter {
            None => Box::new(EnwiroAdapterNone {}),
            Some(adapter_name) => Box::new(EnwiroAdapterExternal::new(adapter_name)),
        };

        Self {
            config,
            reader,
            writer,
            adapter,
        }
    }
}
