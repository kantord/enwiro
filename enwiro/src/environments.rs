use std::collections::HashMap;
use std::fs;

pub struct Environment {
    // Actual path to the environment
    pub path: String,

    // Name should be short enough to be displayed
    pub name: String,
}

impl Environment {
    pub fn get_all(source_directory: &str) -> HashMap<String, Environment> {
        let mut results: HashMap<String, Environment> = HashMap::new();
        let directory_entries = fs::read_dir(source_directory).expect("Could not read workspaces directory. Make sure that the path is a directory you have permissions to access.");

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

        results
    }
}
