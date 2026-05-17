//! Description of an external process for enwiro to spawn.
//!
//! [`ProcessSpec`] holds the program + argv only; environment defaults
//! (cwd, `ENWIRO_ENV`) are applied at materialization, not at
//! construction. Callers pick:
//!
//! - [`ProcessSpec::into_command`] for a plain `std::process::Command`,
//!   no enwiro defaults applied.
//! - [`ProcessSpec::into_command_in_env`] to set `current_dir` to the
//!   env's path and inject `ENWIRO_ENV` on the spawned child.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// Name of the env-var enwiro injects on every child spawned in-env.
/// Public API: stable identifier consumed by core, adapters, garnishes,
/// and any user shell that wants to detect an enwiro env.
pub const ENWIRO_ENV_VAR: &str = "ENWIRO_ENV";

#[derive(Debug, Clone)]
pub struct ProcessSpec {
    program: OsString,
    args: Vec<OsString>,
}

impl ProcessSpec {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
        }
    }

    pub fn arg(mut self, value: impl Into<OsString>) -> Self {
        self.args.push(value.into());
        self
    }

    pub fn args<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(values.into_iter().map(Into::into));
        self
    }

    pub fn into_command(self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        cmd
    }

    pub fn into_command_in_env(self, env_name: &str, env_path: &Path) -> Command {
        let mut cmd = self.into_command();
        cmd.current_dir(env_path).env(ENWIRO_ENV_VAR, env_name);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn into_command_preserves_program_and_args() {
        let cmd = ProcessSpec::new("echo").args(["a", "b c"]).into_command();
        assert_eq!(cmd.get_program(), OsStr::new("echo"));
        let argv: Vec<&OsStr> = cmd.get_args().collect();
        assert_eq!(argv, vec![OsStr::new("a"), OsStr::new("b c")]);
    }

    #[test]
    fn arg_and_args_both_append() {
        let cmd = ProcessSpec::new("cmd")
            .arg("a")
            .args(["b", "c"])
            .arg("d")
            .into_command();
        let argv: Vec<&OsStr> = cmd.get_args().collect();
        assert_eq!(
            argv,
            vec![
                OsStr::new("a"),
                OsStr::new("b"),
                OsStr::new("c"),
                OsStr::new("d"),
            ]
        );
    }

    #[test]
    fn into_command_in_env_sets_cwd_and_enwiro_env() {
        let cmd = ProcessSpec::new("echo").into_command_in_env("myenv", Path::new("/tmp/myenv"));
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/tmp/myenv")));
        let has_env = cmd
            .get_envs()
            .any(|(k, v)| k == OsStr::new("ENWIRO_ENV") && v == Some(OsStr::new("myenv")));
        assert!(has_env, "ENWIRO_ENV not set on the Command");
    }

    #[test]
    fn into_command_plain_does_not_set_cwd_or_enwiro_env() {
        let cmd = ProcessSpec::new("echo").into_command();
        assert!(cmd.get_current_dir().is_none());
        let has_env = cmd.get_envs().any(|(k, _)| k == OsStr::new("ENWIRO_ENV"));
        assert!(!has_env);
    }
}
