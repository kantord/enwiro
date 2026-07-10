use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use enwiro_sdk::adapter::{AdapterCapability, AdapterMetadata, RunPayload};
use enwiro_sdk::cli::AdapterCore;
use enwiro_sdk::process::ENWIRO_ENV_VAR;
use std::os::unix::process::CommandExt;

#[derive(Parser)]
enum EnwiroAdapterTmuxCli {
    #[command(flatten)]
    Core(AdapterCore),
    Listen,
}

/// How often `listen` polls tmux for the current session. tmux has no
/// event push, so this bounds switch-detection latency.
const SESSION_POLL_INTERVAL: Duration = Duration::from_secs(5);

fn validate_session_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("session name must not be empty");
    }
    if name.contains('.') || name.contains(':') {
        anyhow::bail!(
            "session name {:?} contains '.' or ':' which tmux uses as target syntax separators",
            name
        );
    }
    Ok(())
}

fn is_in_tmux(tmux_var: Option<&str>) -> bool {
    tmux_var.is_some_and(|v| !v.is_empty())
}

fn parse_session_name(exit_success: bool, stdout: &str) -> String {
    if exit_success {
        stdout.trim().to_string()
    } else {
        String::new()
    }
}

fn run_tmux(args: &[String]) -> anyhow::Result<std::process::Output> {
    Ok(std::process::Command::new("tmux").args(args).output()?)
}

fn session_exists(name: &str) -> bool {
    let exact = format!("={name}");
    std::process::Command::new("tmux")
        .args(["has-session", "-t", &exact])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn get_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

fn build_new_window_args(payload: &RunPayload) -> Vec<String> {
    let mut args = vec![
        "new-window".to_string(),
        "-t".to_string(),
        format!("={}", payload.env_name),
        "-c".to_string(),
        payload.env_path.clone(),
        "-e".to_string(),
        format!("{}={}", ENWIRO_ENV_VAR, payload.env_name),
        payload.command.clone(),
    ];
    args.extend(payload.args.iter().cloned());
    args
}

fn ensure_session(name: &str) -> anyhow::Result<()> {
    if session_exists(name) {
        return Ok(());
    }
    let shell = get_shell();
    let create_args = new_session_args(name, &shell);
    let output = run_tmux(&create_args)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux new-session failed: {}", stderr);
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let _guard = enwiro_sdk::init_logging("enwiro-adapter-tmux.log");
    let args = EnwiroAdapterTmuxCli::parse();
    match args {
        EnwiroAdapterTmuxCli::Core(AdapterCore::Metadata) => {
            println!(
                "{}",
                AdapterMetadata::with_capabilities([AdapterCapability::Listen]).to_json()
            );
        }
        EnwiroAdapterTmuxCli::Core(AdapterCore::GetActiveWorkspaceId(_)) => {
            let tmux_env = std::env::var("TMUX").ok();
            if !is_in_tmux(tmux_env.as_deref()) {
                print!("");
                return Ok(());
            }
            let output = std::process::Command::new("tmux")
                .args(["display-message", "-p", "#S"])
                .output();
            let (success, stdout) = match output {
                Ok(o) => (
                    o.status.success(),
                    String::from_utf8_lossy(&o.stdout).into_owned(),
                ),
                Err(_) => (false, String::new()),
            };
            print!("{}", parse_session_name(success, &stdout));
        }
        EnwiroAdapterTmuxCli::Core(AdapterCore::Run(_)) => {
            let payload = RunPayload::read_from_stdin()?;
            validate_session_name(&payload.env_name)?;
            ensure_session(&payload.env_name)?;
            let args = build_new_window_args(&payload);
            let output = run_tmux(&args)?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("tmux new-window failed: {}", stderr);
            }
            let tmux_env = std::env::var("TMUX").ok();
            if !is_in_tmux(tmux_env.as_deref()) {
                eprintln!(
                    "Note: not currently attached to tmux. Run `tmux attach -t ={}` to see the new window.",
                    payload.env_name
                );
            }
        }
        EnwiroAdapterTmuxCli::Listen => {
            listen(SESSION_POLL_INTERVAL)?;
        }
        EnwiroAdapterTmuxCli::Core(AdapterCore::Activate(activate_args)) => {
            let name = &activate_args.name;
            validate_session_name(name)?;
            ensure_session(name)?;
            let tmux_env = std::env::var("TMUX").ok();
            if is_in_tmux(tmux_env.as_deref()) {
                let output = run_tmux(&switch_client_args(name))?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    anyhow::bail!("tmux switch-client failed: {}", stderr);
                }
            } else {
                let err = std::process::Command::new("tmux")
                    .args(attach_session_args(name))
                    .exec();
                anyhow::bail!("tmux attach-session failed: {}", err);
            }
        }
    }
    Ok(())
}

fn get_current_session() -> Option<String> {
    let output = std::process::Command::new("tmux")
        .args(["display-message", "-p", "#S"])
        .output()
        .ok()?;
    if output.status.success() {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if name.is_empty() { None } else { Some(name) }
    } else {
        None
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn listen(interval: Duration) -> anyhow::Result<()> {
    let mut last_session: Option<String> = None;

    loop {
        let current = get_current_session();
        if current != last_session {
            if let Some(ref name) = current {
                let event = serde_json::json!({
                    "type": "workspace_switch",
                    "env_name": name,
                    "timestamp": unix_timestamp(),
                });
                println!("{event}");
            }
            last_session = current;
        }
        std::thread::sleep(interval);
    }
}

fn new_session_args(name: &str, shell: &str) -> Vec<String> {
    vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        name.to_string(),
        format!("enw wrap {shell}"),
    ]
}

fn switch_client_args(name: &str) -> Vec<String> {
    vec![
        "switch-client".to_string(),
        "-t".to_string(),
        format!("={name}"),
    ]
}

fn attach_session_args(name: &str) -> Vec<String> {
    vec![
        "attach-session".to_string(),
        "-t".to_string(),
        format!("={name}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_session_name_accepts_plain_name() {
        let result = validate_session_name("my-project");
        assert!(result.is_ok(), "plain name should be accepted");
    }

    #[test]
    fn validate_session_name_rejects_dot() {
        let result = validate_session_name("my.project");
        assert!(
            result.is_err(),
            "name containing '.' should be rejected because tmux uses it as a target syntax separator"
        );
    }

    #[test]
    fn validate_session_name_rejects_colon() {
        let result = validate_session_name("my:project");
        assert!(
            result.is_err(),
            "name containing ':' should be rejected because tmux uses it as a target syntax separator"
        );
    }

    #[test]
    fn validate_session_name_rejects_leading_dot() {
        let result = validate_session_name(".hidden");
        assert!(result.is_err(), "name starting with '.' should be rejected");
    }

    #[test]
    fn validate_session_name_rejects_leading_colon() {
        let result = validate_session_name(":session");
        assert!(result.is_err(), "name starting with ':' should be rejected");
    }

    #[test]
    fn validate_session_name_rejects_trailing_dot() {
        let result = validate_session_name("session.");
        assert!(result.is_err(), "name ending with '.' should be rejected");
    }

    #[test]
    fn validate_session_name_rejects_trailing_colon() {
        let result = validate_session_name("session:");
        assert!(result.is_err(), "name ending with ':' should be rejected");
    }

    #[test]
    fn validate_session_name_accepts_underscores_and_hyphens() {
        let result = validate_session_name("my_project-v2");
        assert!(result.is_ok(), "underscores and hyphens should be accepted");
    }

    #[test]
    fn validate_session_name_accepts_alphanumeric() {
        let result = validate_session_name("project123");
        assert!(result.is_ok(), "alphanumeric names should be accepted");
    }

    #[test]
    fn validate_session_name_rejects_both_dot_and_colon() {
        let result = validate_session_name("my.project:v1");
        assert!(
            result.is_err(),
            "name containing both '.' and ':' should be rejected"
        );
    }

    #[test]
    fn validate_session_name_rejects_empty_name() {
        let result = validate_session_name("");
        assert!(
            result.is_err(),
            "empty name should be rejected — tmux would fail with an opaque error"
        );
    }

    #[test]
    fn parse_session_name_returns_trimmed_stdout_on_success() {
        let result = parse_session_name(true, "my-session\n");
        assert_eq!(
            result, "my-session",
            "when tmux exits successfully the session name should be stdout trimmed of whitespace"
        );
    }

    #[test]
    fn parse_session_name_returns_empty_string_on_failure() {
        let result = parse_session_name(false, "");
        assert_eq!(
            result, "",
            "when tmux fails (not inside tmux) the result should be an empty string"
        );
    }

    #[test]
    fn parse_session_name_ignores_stdout_on_failure() {
        let result = parse_session_name(false, "some-session\n");
        assert_eq!(
            result, "",
            "when tmux exits with failure the stdout content must be ignored and empty string returned"
        );
    }

    #[test]
    fn parse_session_name_trims_leading_and_trailing_whitespace() {
        let result = parse_session_name(true, "  my-session  \n");
        assert_eq!(
            result, "my-session",
            "leading and trailing whitespace in tmux output should be stripped"
        );
    }

    #[test]
    fn parse_session_name_returns_session_name_without_newline() {
        let result = parse_session_name(true, "work\n");
        assert_eq!(
            result, "work",
            "trailing newline from tmux display-message output should be stripped"
        );
    }

    #[test]
    fn is_in_tmux_returns_false_when_tmux_not_set() {
        assert!(
            !is_in_tmux(None),
            "$TMUX unset means not inside a tmux pane"
        );
    }

    #[test]
    fn is_in_tmux_returns_true_when_tmux_set() {
        assert!(
            is_in_tmux(Some("/tmp/tmux-1000/default,1234,0")),
            "$TMUX set to socket path means inside a tmux pane"
        );
    }

    #[test]
    fn is_in_tmux_returns_false_when_tmux_empty() {
        assert!(
            !is_in_tmux(Some("")),
            "$TMUX empty string means not inside a tmux pane"
        );
    }

    #[test]
    fn attach_session_args_first_element_is_attach_session() {
        let args = attach_session_args("myproject");
        assert_eq!(args[0], "attach-session");
    }

    #[test]
    fn attach_session_args_contains_exact_target() {
        let args = attach_session_args("myproject");
        let t_pos = args
            .iter()
            .position(|a| a == "-t")
            .expect("-t must be present");
        assert_eq!(
            args[t_pos + 1],
            "=myproject",
            "target must use '=' prefix for exact matching"
        );
    }

    #[test]
    fn attach_session_args_works_with_different_name() {
        let args = attach_session_args("other-session");
        let t_pos = args.iter().position(|a| a == "-t").unwrap();
        assert_eq!(args[t_pos + 1], "=other-session");
    }

    // Tests for new_session_args

    #[test]
    fn new_session_args_first_element_is_new_session_subcommand() {
        let args = new_session_args("myproject", "/bin/bash");
        assert_eq!(
            args[0], "new-session",
            "first argument must be the 'new-session' tmux subcommand"
        );
    }

    #[test]
    fn new_session_args_contains_detached_flag() {
        let args = new_session_args("myproject", "/bin/bash");
        assert!(
            args.contains(&"-d".to_string()),
            "'-d' flag must be present to start the session in detached mode"
        );
    }

    #[test]
    fn new_session_args_contains_session_name_flag_followed_by_name() {
        let args = new_session_args("myproject", "/bin/bash");
        let s_pos = args.iter().position(|a| a == "-s");
        assert!(
            s_pos.is_some(),
            "'-s' flag must be present to name the new session"
        );
        assert_eq!(
            args[s_pos.unwrap() + 1],
            "myproject",
            "the element after '-s' must be the session name"
        );
    }

    #[test]
    fn new_session_args_last_element_is_enwiro_wrap_shell_command() {
        let args = new_session_args("myproject", "/bin/bash");
        assert_eq!(
            args.last().unwrap(),
            "enw wrap /bin/bash",
            "last argument must be the startup command 'enw wrap <shell>'"
        );
    }

    #[test]
    fn new_session_args_works_with_different_name_and_shell() {
        let args = new_session_args("myproject", "/bin/zsh");
        let s_pos = args.iter().position(|a| a == "-s").unwrap();
        assert_eq!(
            args[s_pos + 1],
            "myproject",
            "session name must match the provided name argument"
        );
        assert_eq!(
            args.last().unwrap(),
            "enw wrap /bin/zsh",
            "startup command must embed the provided shell path"
        );
    }

    // Tests for switch_client_args

    #[test]
    fn switch_client_args_first_element_is_switch_client_subcommand() {
        let args = switch_client_args("myproject");
        assert_eq!(
            args[0], "switch-client",
            "first argument must be the 'switch-client' tmux subcommand"
        );
    }

    #[test]
    fn switch_client_args_contains_target_flag_followed_by_name() {
        let args = switch_client_args("myproject");
        let t_pos = args.iter().position(|a| a == "-t");
        assert!(
            t_pos.is_some(),
            "'-t' flag must be present to specify the target session"
        );
        assert_eq!(
            args[t_pos.unwrap() + 1],
            "=myproject",
            "the element after '-t' must be the exact session name prefixed with '=' to prevent fuzzy matching"
        );
    }

    #[test]
    fn switch_client_args_works_with_different_name() {
        let args = switch_client_args("other-session");
        let t_pos = args.iter().position(|a| a == "-t").unwrap();
        assert_eq!(
            args[t_pos + 1],
            "=other-session",
            "session name must be prefixed with '=' for exact tmux target matching"
        );
    }
}
