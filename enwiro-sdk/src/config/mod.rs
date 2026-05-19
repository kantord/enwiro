//! Layered config loader for cookbook plugins.
//!
//! Trusted core (`enw` CLI + daemon) calls this; cookbooks never parse
//! TOML files. The loader produces a `serde_json::Value` that gets
//! shipped to cookbooks as the `config` field of a [`CookbookPayload`].
//!
//! Layering (later wins):
//!
//! 1. User-level `~/.config/enwiro/<scope>.toml` — unfiltered. Matches
//!    the path confy used so existing user files keep working.
//! 2. Project-level `.enwiro.toml` files walking up from `cwd`,
//!    outermost-ancestor first → innermost wins. Each layer's
//!    `[<scope>]` section is filtered through `allowlist` before merge.
//!
//! Merge is delegated to `config-rs`. Project layers are pre-filtered
//! through the `toml` crate to drop non-allowlisted keys, then handed
//! back to `config-rs` as in-memory string sources. This keeps the
//! merge implementation in one well-known library while letting us
//! enforce the per-layer allowlist policy that `config-rs` doesn't
//! natively support.
//!
//! Malformed user-level TOML fails loud (matches confy's previous
//! behavior). Malformed project-level TOML at any single layer is
//! logged and skipped — one botched repo file can't break daemon reads.
//!
//! [`CookbookPayload`]: crate::cookbook::CookbookPayload

use anyhow::{Context, Result};
use config::{Config, File, FileFormat};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

const PROJECT_FILE_NAME: &str = ".enwiro.toml";
const USER_CONFIG_SUBDIR: &str = ".config/enwiro";

pub struct ConfigLoader {
    home: PathBuf,
}

impl ConfigLoader {
    pub fn from_env() -> Result<Self> {
        let home = home::home_dir().context("Could not determine user home directory")?;
        Ok(Self { home })
    }

    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    pub fn user_config_path(&self, scope: &str) -> PathBuf {
        self.home
            .join(USER_CONFIG_SUBDIR)
            .join(format!("{scope}.toml"))
    }

    pub fn load_user_config(&self, scope: &str) -> Result<Value> {
        let path = self.user_config_path(scope);
        if !path.exists() {
            return Ok(Value::Object(Map::new()));
        }
        let cfg = Config::builder()
            .add_source(File::from(path.as_path()).format(FileFormat::Toml))
            .build()
            .with_context(|| format!("Failed to load user config at {}", path.display()))?;
        cfg.try_deserialize::<Value>()
            .with_context(|| format!("Failed to deserialize user config at {}", path.display()))
    }

    pub fn build_cookbook_config(
        &self,
        cwd: &Path,
        scope: &str,
        allowlist: &[&str],
    ) -> Result<Value> {
        let mut builder = Config::builder();

        let user_path = self.user_config_path(scope);
        if user_path.exists() {
            builder = builder.add_source(File::from(user_path.as_path()).format(FileFormat::Toml));
        }

        for path in collect_project_files(cwd) {
            let Some(filtered) = filter_project_layer(&path, scope, allowlist) else {
                continue;
            };
            builder = builder.add_source(File::from_str(&filtered, FileFormat::Toml));
        }

        let cfg = builder.build().context("Failed to merge config layers")?;
        cfg.try_deserialize::<Value>()
            .context("Failed to deserialize merged config")
    }
}

/// Free-function shim using the real user home directory.
pub fn build_cookbook_config(cwd: &Path, scope: &str, allowlist: &[&str]) -> Result<Value> {
    ConfigLoader::from_env()?.build_cookbook_config(cwd, scope, allowlist)
}

/// Free-function shim using the real user home directory.
pub fn load_user_config(scope: &str) -> Result<Value> {
    ConfigLoader::from_env()?.load_user_config(scope)
}

/// Free-function shim using the real user home directory.
pub fn user_config_path(scope: &str) -> Result<PathBuf> {
    Ok(ConfigLoader::from_env()?.user_config_path(scope))
}

fn collect_project_files(cwd: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = cwd
        .ancestors()
        .map(|dir| dir.join(PROJECT_FILE_NAME))
        .filter(|p| p.is_file())
        .collect();
    files.reverse();
    files
}

/// Read a project `.enwiro.toml`, extract its `[<scope>]` section, drop
/// keys not in `allowlist`, and re-serialize as a TOML string ready to
/// hand to `config-rs` as a source. Returns `None` (with a debug log)
/// on read/parse error, missing section, or non-table section.
fn filter_project_layer(path: &Path, scope: &str, allowlist: &[&str]) -> Option<String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "Failed to read project config; skipping");
            return None;
        }
    };
    let parsed: toml::Table = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "Malformed TOML in project config; skipping");
            return None;
        }
    };
    let section = parsed.get(scope)?.as_table().or_else(|| {
        tracing::debug!(
            path = %path.display(),
            scope,
            "Project config section is not a TOML table; skipping"
        );
        None
    })?;

    let mut kept = toml::Table::new();
    for (k, v) in section {
        if allowlist.contains(&k.as_str()) {
            kept.insert(k.clone(), v.clone());
        } else {
            tracing::debug!(scope, key = %k, "Dropping non-allowlisted project config key");
        }
    }

    toml::to_string(&kept).ok().or_else(|| {
        tracing::debug!(path = %path.display(), "Failed to re-serialize filtered TOML; skipping");
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    struct Env {
        home: TempDir,
        cwd: TempDir,
    }

    impl Env {
        fn new() -> Self {
            Self {
                home: tempfile::tempdir().expect("home tempdir"),
                cwd: tempfile::tempdir().expect("cwd tempdir"),
            }
        }

        fn loader(&self) -> ConfigLoader {
            ConfigLoader::with_home(self.home.path())
        }

        fn write_user(&self, scope: &str, body: &str) {
            let dir = self.home.path().join(USER_CONFIG_SUBDIR);
            fs::create_dir_all(&dir).expect("mkdir user config dir");
            fs::write(dir.join(format!("{scope}.toml")), body).expect("write user config");
        }

        fn write_project(&self, dir: &Path, body: &str) {
            fs::create_dir_all(dir).expect("mkdir project dir");
            fs::write(dir.join(PROJECT_FILE_NAME), body).expect("write project config");
        }
    }

    #[test]
    fn user_config_path_matches_confy_layout() {
        let env = Env::new();
        let p = env.loader().user_config_path("cookbook-git");
        assert_eq!(p, env.home.path().join(".config/enwiro/cookbook-git.toml"));
    }

    #[test]
    fn load_user_config_returns_empty_when_missing() {
        let env = Env::new();
        let v = env
            .loader()
            .load_user_config("cookbook-git")
            .expect("missing file is not an error");
        assert_eq!(v, json!({}));
    }

    #[test]
    fn load_user_config_parses_valid_toml() {
        let env = Env::new();
        env.write_user("cookbook-git", "repo_globs = [\"a\", \"b\"]\n");
        let v = env.loader().load_user_config("cookbook-git").unwrap();
        assert_eq!(v, json!({ "repo_globs": ["a", "b"] }));
    }

    #[test]
    fn load_user_config_fails_loud_on_malformed_toml() {
        let env = Env::new();
        env.write_user("cookbook-git", "this is not = = valid toml");
        let err = env
            .loader()
            .load_user_config("cookbook-git")
            .expect_err("malformed user TOML must propagate");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("user config"),
            "error should mention user config path; got: {msg}"
        );
    }

    #[test]
    fn no_project_files_yields_user_only() {
        let env = Env::new();
        env.write_user("cookbook-git", "repo_globs = [\"u\"]\n");
        let v = env
            .loader()
            .build_cookbook_config(env.cwd.path(), "cookbook-git", &["repo_globs"])
            .unwrap();
        assert_eq!(v, json!({ "repo_globs": ["u"] }));
    }

    #[test]
    fn project_layer_wins_for_allowlisted_keys() {
        let env = Env::new();
        env.write_user("cookbook-git", "repo_globs = [\"u\"]\n");
        env.write_project(env.cwd.path(), "[cookbook-git]\nrepo_globs = [\"p\"]\n");
        let v = env
            .loader()
            .build_cookbook_config(env.cwd.path(), "cookbook-git", &["repo_globs"])
            .unwrap();
        assert_eq!(v, json!({ "repo_globs": ["p"] }));
    }

    #[test]
    fn innermost_project_layer_wins_over_outer() {
        let env = Env::new();
        let inner = env.cwd.path().join("packages/a");
        env.write_project(env.cwd.path(), "[cookbook-git]\nrepo_globs = [\"outer\"]\n");
        env.write_project(&inner, "[cookbook-git]\nrepo_globs = [\"inner\"]\n");
        let v = env
            .loader()
            .build_cookbook_config(&inner, "cookbook-git", &["repo_globs"])
            .unwrap();
        assert_eq!(v, json!({ "repo_globs": ["inner"] }));
    }

    #[test]
    fn non_allowlisted_keys_dropped() {
        let env = Env::new();
        env.write_user("cookbook-git", "repo_globs = [\"u\"]\n");
        env.write_project(
            env.cwd.path(),
            "[cookbook-git]\nrepo_globs = [\"p\"]\nworkspaces_directory = \"/hostile\"\n",
        );
        let v = env
            .loader()
            .build_cookbook_config(env.cwd.path(), "cookbook-git", &["repo_globs"])
            .unwrap();
        assert_eq!(v, json!({ "repo_globs": ["p"] }));
    }

    #[test]
    fn empty_allowlist_makes_project_file_a_noop() {
        let env = Env::new();
        env.write_user("cookbook-git", "repo_globs = [\"u\"]\n");
        env.write_project(env.cwd.path(), "[cookbook-git]\nrepo_globs = [\"p\"]\n");
        let v = env
            .loader()
            .build_cookbook_config(env.cwd.path(), "cookbook-git", &[])
            .unwrap();
        assert_eq!(v, json!({ "repo_globs": ["u"] }));
    }

    #[test]
    fn malformed_project_layer_logged_and_skipped_other_layers_keep_working() {
        let env = Env::new();
        let inner = env.cwd.path().join("packages/a");
        env.write_user("cookbook-git", "repo_globs = [\"u\"]\n");
        env.write_project(env.cwd.path(), "this is = = not valid toml");
        env.write_project(&inner, "[cookbook-git]\nrepo_globs = [\"inner\"]\n");
        let v = env
            .loader()
            .build_cookbook_config(&inner, "cookbook-git", &["repo_globs"])
            .unwrap();
        assert_eq!(v, json!({ "repo_globs": ["inner"] }));
    }

    #[test]
    fn missing_scope_section_is_silent_noop() {
        let env = Env::new();
        env.write_user("cookbook-git", "repo_globs = [\"u\"]\n");
        env.write_project(env.cwd.path(), "[cookbook-github]\nsomething = \"x\"\n");
        let v = env
            .loader()
            .build_cookbook_config(env.cwd.path(), "cookbook-git", &["repo_globs"])
            .unwrap();
        assert_eq!(v, json!({ "repo_globs": ["u"] }));
    }

    #[test]
    fn nested_table_keys_merge_deeply() {
        let env = Env::new();
        env.write_user("cookbook-git", "[settings]\nfoo = 1\nbar = 2\n");
        env.write_project(env.cwd.path(), "[cookbook-git.settings]\nbar = 99\n");
        let v = env
            .loader()
            .build_cookbook_config(env.cwd.path(), "cookbook-git", &["settings"])
            .unwrap();
        assert_eq!(v, json!({ "settings": { "foo": 1, "bar": 99 } }));
    }
}
