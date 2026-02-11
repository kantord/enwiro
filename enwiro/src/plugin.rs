use std::collections::HashSet;
use std::path::{Path, PathBuf};

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

pub fn get_search_directories() -> Vec<PathBuf> {
    let mut dirs = vec![];

    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            dirs.push(dir);
        }
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        dirs.push(parent.to_path_buf());
    }

    dirs
}

pub fn find_plugins_in_directory(dir: &Path, plugin_kind: &PluginKind) -> HashSet<Plugin> {
    let mut results = HashSet::new();
    let expected_prefix = format!("enwiro-{}-", plugin_kind).to_lowercase();

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return results,
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if let Some(plugin_name) = name.strip_prefix(&expected_prefix) {
            results.insert(Plugin {
                name: plugin_name.to_string(),
                kind: plugin_kind.clone(),
                executable: entry.path().to_string_lossy().to_string(),
            });
        }
    }

    results
}

pub fn get_plugins(plugin_kind: PluginKind) -> HashSet<Plugin> {
    let mut results = HashSet::new();
    for dir in get_search_directories() {
        results.extend(find_plugins_in_directory(&dir, &plugin_kind));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_search_directories_includes_exe_parent() {
        let dirs = get_search_directories();
        let exe_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert!(
            dirs.contains(&exe_dir),
            "exe dir {:?} should be in search directories, but got {:?}",
            exe_dir,
            dirs
        );
    }

    #[test]
    fn test_find_plugins_returns_full_path_as_executable() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::write(
            temp_dir.path().join("enwiro-cookbook-fakeplugin"),
            "#!/bin/sh\n",
        )
        .unwrap();

        let plugins = find_plugins_in_directory(temp_dir.path(), &PluginKind::Cookbook);
        assert_eq!(plugins.len(), 1);

        let plugin = plugins.into_iter().next().unwrap();
        assert_eq!(plugin.name, "fakeplugin");
        assert!(
            Path::new(&plugin.executable).is_absolute(),
            "executable should be an absolute path, but got {:?}",
            plugin.executable
        );
    }
}
