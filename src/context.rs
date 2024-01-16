use crate::config::ConfigurationValues;
use std::io::{Read, Write};

pub struct CommandContext<R: Read, W: Write> {
    pub config: ConfigurationValues,
    pub reader: R,
    pub writer: W,
}
