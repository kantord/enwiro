use anyhow::{Context, bail};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Environment {
    // Actual path to the environment
    pub path: String,

    // Name should be short enough to be displayed
    pub name: String,
}

/// Which entry inside `env_dir` (the "new format" per-env directory) is
/// this environment's project directory. Prefers `main_folder` from
/// meta.json; every env cooked from here on always has it set
/// (`context.rs::save_cook_metadata`). For a plain env that is a symlink to
/// the cooked project; for a composed env (#375) it is a real directory
/// (the wrapper folder holding one symlink per part).
///
/// TODO(by 2026-09): the same-named-symlink fallback below only exists for
/// envs cooked before main_folder was written unconditionally. Once enough
/// time has passed that no such env is still in use, delete the fallback and
/// require main_folder to resolve.
fn resolve_project_symlink(env_dir: &Path, id: &str) -> Option<PathBuf> {
    let meta = enwiro_daemon::meta::load_env_meta(env_dir);
    if let Some(main_folder) = meta.main_folder
        && is_plain_component(&main_folder)
    {
        let candidate = env_dir.join(main_folder);
        if candidate.is_symlink() || candidate.is_dir() {
            return Some(candidate);
        }
    }
    let default = env_dir.join(id);
    default.is_symlink().then_some(default)
}

/// Whether `name` is safe to join onto `env_dir` as a single path segment
/// (no traversal outside it): non-empty, no path separator, and not `.`/`..`.
fn is_plain_component(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && name != "." && name != ".."
}

impl Environment {
    pub fn get_all(source_directory: &str) -> anyhow::Result<HashMap<String, Environment>> {
        let mut results: HashMap<String, Environment> = HashMap::new();
        let directory_entries = fs::read_dir(source_directory)?;

        for directory_entry in directory_entries {
            let entry = directory_entry.context("Failed to read directory entry")?;
            let path = entry.path();
            let id = path
                .file_name()
                .context("Failed to get file name")?
                .to_str()
                .context("Failed to convert file name to string")?
                .to_string();

            let metadata = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            if metadata.file_type().is_symlink() && path.is_dir() {
                // Legacy bare symlink pointing to a directory
                let new_environment = Environment {
                    path: path
                        .to_str()
                        .context("Failed to convert path to string")?
                        .to_string(),
                    name: id.clone(),
                };
                results.insert(id, new_environment);
            } else if metadata.file_type().is_dir() {
                // New format: directory containing an inner project symlink
                // (same-named by default; see `resolve_project_symlink`).
                if let Some(inner_path) = resolve_project_symlink(&path, &id) {
                    let new_environment = Environment {
                        path: inner_path
                            .to_str()
                            .context("Failed to convert inner path to string")?
                            .to_string(),
                        name: id.clone(),
                    };
                    results.insert(id, new_environment);
                }
            }
        }

        Ok(results)
    }

    pub fn get_one(source_directory: &str, name: &str) -> anyhow::Result<Environment> {
        let mut environments = Self::get_all(source_directory)?;

        match environments.remove(name) {
            Some(x) => Ok(x),
            None => bail!("Environment \"{}\" does not exist", name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enwiro_daemon::meta::EnvStats;

    fn new_format_env(root: &Path, name: &str) -> PathBuf {
        let env_dir = root.join(name);
        fs::create_dir_all(&env_dir).unwrap();
        env_dir
    }

    fn symlink_to_fresh_dir(env_dir: &Path, link_name: &str) -> PathBuf {
        let target = env_dir.join(format!("{link_name}-target"));
        fs::create_dir(&target).unwrap();
        let link = env_dir.join(link_name);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        target
    }

    fn write_main_folder(env_dir: &Path, main_folder: &str) {
        let meta = EnvStats {
            main_folder: Some(main_folder.to_string()),
            ..Default::default()
        };
        fs::write(
            env_dir.join("meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn get_all_defaults_to_the_same_named_symlink_without_main_folder() {
        let root = tempfile::tempdir().unwrap();
        let env_dir = new_format_env(root.path(), "my-env");
        symlink_to_fresh_dir(&env_dir, "my-env");

        let environments = Environment::get_all(root.path().to_str().unwrap()).unwrap();

        assert_eq!(
            environments["my-env"].path,
            env_dir.join("my-env").to_str().unwrap()
        );
    }

    #[test]
    fn get_all_uses_main_folder_when_present() {
        let root = tempfile::tempdir().unwrap();
        let env_dir = new_format_env(root.path(), "my-env");
        symlink_to_fresh_dir(&env_dir, "my-env");
        symlink_to_fresh_dir(&env_dir, "sub");
        write_main_folder(&env_dir, "sub");

        let environments = Environment::get_all(root.path().to_str().unwrap()).unwrap();

        assert_eq!(
            environments["my-env"].path,
            env_dir.join("sub").to_str().unwrap()
        );
    }

    #[test]
    fn get_all_accepts_a_real_directory_as_main_folder() {
        // Composed environments (#375): the project directory is a real
        // wrapper folder holding one symlink per part, not itself a symlink.
        let root = tempfile::tempdir().unwrap();
        let env_dir = new_format_env(root.path(), "foo+bar");
        fs::create_dir(env_dir.join("foo+bar")).unwrap();
        write_main_folder(&env_dir, "foo+bar");

        let environments = Environment::get_all(root.path().to_str().unwrap()).unwrap();

        assert_eq!(
            environments["foo+bar"].path,
            env_dir.join("foo+bar").to_str().unwrap()
        );
    }

    #[test]
    fn get_all_falls_back_to_default_when_main_folder_does_not_resolve() {
        let root = tempfile::tempdir().unwrap();
        let env_dir = new_format_env(root.path(), "my-env");
        symlink_to_fresh_dir(&env_dir, "my-env");
        write_main_folder(&env_dir, "does-not-exist");

        let environments = Environment::get_all(root.path().to_str().unwrap()).unwrap();

        assert_eq!(
            environments["my-env"].path,
            env_dir.join("my-env").to_str().unwrap()
        );
    }

    #[test]
    fn get_all_falls_back_to_default_when_main_folder_attempts_traversal() {
        let root = tempfile::tempdir().unwrap();
        let env_dir = new_format_env(root.path(), "my-env");
        symlink_to_fresh_dir(&env_dir, "my-env");
        // A symlink that genuinely lives outside env_dir -- if main_folder's
        // traversal guard were missing, this would resolve to it instead of
        // falling back to the default same-named symlink.
        symlink_to_fresh_dir(root.path(), "escaped");
        write_main_folder(&env_dir, "../escaped");

        let environments = Environment::get_all(root.path().to_str().unwrap()).unwrap();

        assert_eq!(
            environments["my-env"].path,
            env_dir.join("my-env").to_str().unwrap()
        );
    }
}
