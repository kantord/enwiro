use std::collections::HashMap;
use std::{fs, io};

pub struct Environment {
    // Actual path to the environment
    pub path: String,

    // Name should be short enough to be displayed
    pub name: String,
}

impl Environment {
    pub fn get_all(source_directory: &str) -> Result<HashMap<String, Environment>, io::Error> {
        let mut results: HashMap<String, Environment> = HashMap::new();
        let directory_entries = fs::read_dir(source_directory)?;

        for directory_entry in directory_entries {
            let path = directory_entry.unwrap().path();
            let id = path.file_name().unwrap().to_str().unwrap().to_string();

            if path.is_dir() {
                let new_environment = Environment {
                    path: path.to_str().unwrap().to_string(),
                    name: id.clone(),
                };

                results.insert(id.clone(), new_environment);
            }
        }

        Ok(results)
    }

    pub fn get_one(source_directory: &str, name: &str) -> Result<Environment, io::Error> {
        let mut environments = Self::get_all(source_directory)?;

        match environments.remove(name) {
            Some(x) => Ok(x),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Environment \"{}\" does not exist", name),
            ))?,
        }
    }
}
