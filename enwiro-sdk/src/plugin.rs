use std::collections::HashSet;
use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(strum_macros::Display, Hash, Eq, PartialEq, Clone, Debug)]
pub enum PluginKind {
    Adapter,
    Cookbook,
    Garnish,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginName(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidPluginName(String);

impl fmt::Display for InvalidPluginName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid plugin name: {}", self.0)
    }
}

impl std::error::Error for InvalidPluginName {}

impl PluginName {
    pub fn new(s: impl Into<String>) -> Result<Self, InvalidPluginName> {
        let name = s.into();
        if name.is_empty() {
            return Err(InvalidPluginName("must not be empty".into()));
        }
        if name == "." || name == ".." {
            return Err(InvalidPluginName(format!("'{name}' is reserved")));
        }
        if name.contains('/') || name.contains('\\') {
            return Err(InvalidPluginName("must not contain path separators".into()));
        }
        if name.contains('\0') {
            return Err(InvalidPluginName("must not contain null bytes".into()));
        }
        if name.len() > 242 {
            return Err(InvalidPluginName(format!(
                "must be at most 242 bytes, got {}",
                name.len()
            )));
        }
        Ok(PluginName(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn gear_filename(&self) -> String {
        format!("cookbook-{}.json", self.0)
    }
}

impl fmt::Display for PluginName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for PluginName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Hash, Eq, PartialEq, Debug)]
pub struct Plugin {
    pub name: PluginName,
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
        Err(e) => {
            tracing::debug!(dir = ?dir, "Could not read plugin directory: {}", e);
            return results;
        }
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if let Some(plugin_name) = name.strip_prefix(&expected_prefix) {
            let is_executable = entry
                .metadata()
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false);
            if !is_executable {
                continue;
            }
            let validated_name = match PluginName::new(plugin_name) {
                Ok(n) => n,
                Err(e) => {
                    tracing::debug!(name = %plugin_name, error = %e, "Skipping plugin with invalid name");
                    continue;
                }
            };
            tracing::debug!(name = %validated_name, path = %entry.path().display(), "Found plugin");
            results.insert(Plugin {
                name: validated_name,
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
    tracing::debug!(count = results.len(), kind = %plugin_kind, "Plugin discovery complete");
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
    fn test_find_plugins_skips_non_executable_files() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Non-executable file with matching prefix (e.g. a .d dependency file)
        fs::write(
            temp_dir.path().join("enwiro-cookbook-fakeplugin.d"),
            "dependency info\n",
        )
        .unwrap();

        let plugins = find_plugins_in_directory(temp_dir.path(), &PluginKind::Cookbook);
        assert_eq!(
            plugins.len(),
            0,
            "non-executable files should not be picked up as plugins, but got {:?}",
            plugins
        );
    }

    #[test]
    fn test_find_plugins_returns_full_path_as_executable() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::tempdir().unwrap();
        let plugin_path = temp_dir.path().join("enwiro-cookbook-fakeplugin");
        fs::write(&plugin_path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&plugin_path, fs::Permissions::from_mode(0o755)).unwrap();

        let plugins = find_plugins_in_directory(temp_dir.path(), &PluginKind::Cookbook);
        assert_eq!(plugins.len(), 1);

        let plugin = plugins.into_iter().next().unwrap();
        assert_eq!(plugin.name.as_str(), "fakeplugin");
        assert!(
            Path::new(&plugin.executable).is_absolute(),
            "executable should be an absolute path, but got {:?}",
            plugin.executable
        );
    }

    mod plugin_name {
        use super::*;

        #[test]
        fn accepts_valid_names() {
            assert!(PluginName::new("git").is_ok());
            assert!(PluginName::new("my-cookbook").is_ok());
            assert!(PluginName::new("a").is_ok());
        }

        #[test]
        fn rejects_empty() {
            assert!(PluginName::new("").is_err());
        }

        #[test]
        fn rejects_path_separators() {
            assert!(PluginName::new("foo/bar").is_err());
            assert!(PluginName::new("foo\\bar").is_err());
        }

        #[test]
        fn rejects_null_bytes() {
            assert!(PluginName::new("foo\0bar").is_err());
        }

        #[test]
        fn rejects_dot_and_dotdot() {
            assert!(PluginName::new(".").is_err());
            assert!(PluginName::new("..").is_err());
        }

        #[test]
        fn rejects_overly_long_names() {
            let long = "a".repeat(243);
            assert!(PluginName::new(long).is_err());
        }

        #[test]
        fn gear_filename_produces_expected_format() {
            let pn = PluginName::new("git").unwrap();
            assert_eq!(pn.gear_filename(), "cookbook-git.json");
        }
    }

    mod plugin_name_props {
        use proptest::prelude::*;

        use super::*;

        fn invalid_name_strategy() -> impl Strategy<Value = String> {
            prop_oneof![
                Just(String::new()),
                Just(".".to_string()),
                Just("..".to_string()),
                ".*/.+",
                ".*\\\\.+",
                ".+\0.+",
                "[a-z]{243,300}",
            ]
        }

        proptest! {
            #[test]
            fn accepts_any_valid_name(name in "[a-zA-Z0-9_-]{1,242}") {
                prop_assert!(PluginName::new(&name).is_ok());
            }

            #[test]
            fn rejects_every_invalid_name(name in invalid_name_strategy()) {
                prop_assert!(
                    PluginName::new(&name).is_err(),
                    "expected rejection for {:?}", name
                );
            }

            #[test]
            fn valid_names_produce_sanitized_gear_filenames(name in "[a-zA-Z0-9_-]{1,50}") {
                let pn = PluginName::new(&name).unwrap();
                let filename = pn.gear_filename();
                let sanitized = sanitize_filename::sanitize(&filename);
                prop_assert_eq!(&filename, &sanitized);
            }

            #[test]
            fn as_str_roundtrips(name in "[a-zA-Z0-9_-]{1,50}") {
                let pn = PluginName::new(&name).unwrap();
                prop_assert_eq!(pn.as_str(), name.as_str());
            }
        }
    }
}
