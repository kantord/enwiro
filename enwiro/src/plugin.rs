use std::collections::HashSet;

use path_lookup::iterate_executables;

#[derive(strum_macros::Display, Hash, Eq, PartialEq, Clone, Debug)]
pub enum PluginKind {
    Adapter,
    Cookbook,
}

#[derive(Hash, Eq, PartialEq, Debug)]
pub struct Plugin {
    pub name: String,
    pub kind: PluginKind,
    pub executable: String,
}

pub fn get_plugins(plugin_kind: PluginKind) -> HashSet<Plugin> {
    let mut results = HashSet::new();
    let expected_prefix = format!("enwiro-{}-", plugin_kind.to_string())
        .to_string()
        .to_lowercase();

    for executable in iterate_executables() {
        if executable.starts_with(&expected_prefix) {
            results.insert(Plugin {
                name: executable
                    .strip_prefix(&expected_prefix)
                    .unwrap()
                    .to_string(),
                kind: plugin_kind.clone(),
                executable,
            });
        }
    }

    results
}
