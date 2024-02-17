use crate::{
    commands::adapter::{EnwiroAdapterExternal, EnwiroAdapterNone, EnwiroAdapterTrait},
    config::ConfigurationValues,
    environments::Environment,
};
use std::io::{Read, Write};

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

    pub fn get_environment(&self, name: Option<String>) -> Result<Environment, std::io::Error> {
        let selected_environment_name = match name {
            Some(x) => x,
            None => self.adapter.get_active_environment_name().unwrap(),
        };

        return Environment::get_one(
            &self.config.workspaces_directory,
            &selected_environment_name,
        );
    }
}
