use std::process::Command;

pub trait EnwiroAdapterTrait {
    fn get_active_environment_name(&self) -> Result<String, std::io::Error>;
    fn get_active_lens_name(&self) -> Result<String, std::io::Error>;
}

pub struct EnwiroAdapterExternal {
    adapter_command: String,
}

impl EnwiroAdapterTrait for EnwiroAdapterExternal {
    fn get_active_environment_name(&self) -> Result<String, std::io::Error> {
        let output = Command::new(&self.adapter_command)
            .arg("get-active-workspace-id")
            .output()
            .expect("Adapter failed to determine active environment name");

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Ok(stdout.to_string().split(':').nth(0).unwrap().to_string());
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            panic!("Error: {}", stderr);
        }
    }

    fn get_active_lens_name(&self) -> Result<String, std::io::Error> {
        let output = Command::new(&self.adapter_command)
            .arg("get-active-workspace-id")
            .output()
            .expect("Adapter failed to determine active lens name");

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            match stdout.to_string().split(':').nth(1) {
                Some(value) => Ok(value.to_string()),
                None => Ok("".to_string()),
            }
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
    fn get_active_environment_name(&self) -> Result<String, std::io::Error> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine active environment because no adapter is configured.",
        ))
    }

    fn get_active_lens_name(&self) -> Result<String, std::io::Error> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine active lens because no adapter is configured.",
        ))
    }
}
