use anyhow::{Context, anyhow};

use crate::CommandContext;
use crate::context::CookConfig;

use std::os::unix::process::CommandExt;
use std::{env, io::Write, process::Command};

#[cfg(feature = "container-wrap")]
use std::{io::IsTerminal, process::Stdio};

/// Prefix for the per-environment OCI image tag. Stage 0 of the isolation
/// layer (issue #540) triggers purely on the *presence* of a prebuilt image
/// named `enwiro/<env-name>`; building that image is out-of-band for now.
#[cfg(feature = "container-wrap")]
const CONTAINER_IMAGE_PREFIX: &str = "enwiro/";

/// Container engines enwiro knows how to drive, in preference order. Podman
/// first (rootless, daemonless), Docker as the fallback.
#[cfg(feature = "container-wrap")]
const CONTAINER_ENGINES: [&str; 2] = ["podman", "docker"];

#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "Run an application/command inside an environment"
)]
pub struct WrapArgs {
    pub command_name: String,
    pub environment_name: Option<String>,

    #[clap(allow_hyphen_values = true, num_args = 0.., last=true)]
    child_args: Option<Vec<String>>,
}

pub fn wrap<W: Write>(context: &mut CommandContext<W>, args: WrapArgs) -> anyhow::Result<()> {
    let selected_environment =
        match context.get_or_cook_environment(&args.environment_name, &CookConfig::default()) {
            Ok(env) => Some(env),
            Err(e) => {
                tracing::warn!(error = %e, "Could not resolve environment");
                None
            }
        };

    let environment_path: String = match &selected_environment {
        Some(environment) => environment.path.clone(),
        None => {
            tracing::warn!("No matching environment found, falling back to home directory");

            home::home_dir()
                .context("Could not determine user home directory")?
                .into_os_string()
                .into_string()
                .map_err(|_| anyhow!("Could not convert home directory path to string"))?
        }
    };

    let environment_name: String = match &selected_environment {
        Some(environment) => environment.name.clone(),
        None => String::from(""),
    };

    let child_args: Vec<String> = args.child_args.unwrap_or_default();

    // Stage 0 isolation (behind the `container-wrap` build-time feature): if a
    // prebuilt image exists for this environment, exec the command inside it and
    // never return. With the feature off, this is a no-op and we fall through to
    // the unchanged host path below.
    maybe_exec_in_container(
        selected_environment.is_some(),
        &environment_path,
        &environment_name,
        &args.command_name,
        &child_args,
    )?;

    // Host path: the original behaviour, unchanged.
    env::set_current_dir(environment_path).context("Failed to change directory")?;

    let err = Command::new(&args.command_name)
        .env("ENWIRO_ENV", environment_name)
        .args(child_args)
        .exec();

    Err(anyhow!(err).context(format!("Failed to exec {}", args.command_name)))
}

/// Container path is compiled out entirely unless the `container-wrap` feature
/// is enabled, so default builds carry zero container logic or surface.
#[cfg(not(feature = "container-wrap"))]
fn maybe_exec_in_container(
    _environment_resolved: bool,
    _environment_path: &str,
    _environment_name: &str,
    _command: &str,
    _child_args: &[String],
) -> anyhow::Result<()> {
    Ok(())
}

/// When a prebuilt image exists for a resolved environment, `exec` the command
/// inside it via the first available engine (never returns on success).
/// Otherwise returns `Ok(())` so the caller proceeds with the host path.
#[cfg(feature = "container-wrap")]
fn maybe_exec_in_container(
    environment_resolved: bool,
    environment_path: &str,
    environment_name: &str,
    command: &str,
    child_args: &[String],
) -> anyhow::Result<()> {
    // The home-dir fallback has no associated image, so only a real resolved
    // environment is eligible for containerization.
    if !environment_resolved {
        return Ok(());
    }
    let Some(engine) = find_container_engine() else {
        return Ok(());
    };
    let image = container_image_tag(environment_name);
    if !image_exists(engine, &image) {
        return Ok(());
    }

    let argv = build_container_argv(
        engine,
        &image,
        environment_path,
        environment_name,
        command,
        child_args,
        std::io::stdin().is_terminal(),
    );
    tracing::info!(engine, %image, "Running command inside container");
    let err = Command::new(&argv[0]).args(&argv[1..]).exec();
    Err(anyhow!(err).context(format!("Failed to exec {engine}")))
}

/// The OCI image tag enwiro looks for to containerize a given environment.
#[cfg(feature = "container-wrap")]
fn container_image_tag(environment_name: &str) -> String {
    format!("{CONTAINER_IMAGE_PREFIX}{environment_name}")
}

/// First available container engine on PATH, in preference order, or None.
#[cfg(feature = "container-wrap")]
fn find_container_engine() -> Option<&'static str> {
    CONTAINER_ENGINES
        .into_iter()
        .find(|engine| binary_on_path(engine))
}

/// True iff a file named `name` exists in any PATH directory. Good enough to
/// pick an engine; the actual exec resolves it through PATH again.
#[cfg(feature = "container-wrap")]
fn binary_on_path(name: &str) -> bool {
    let Ok(path) = env::var("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

/// Ask the engine whether `image` is present locally. Podman has a dedicated
/// `image exists` (exit 0/1); Docker has no such verb, so we fall back to
/// `image inspect` (exit 0 iff present). Any spawn error counts as "absent".
#[cfg(feature = "container-wrap")]
fn image_exists(engine: &str, image: &str) -> bool {
    let probe_args: [&str; 3] = match engine {
        "podman" => ["image", "exists", image],
        _ => ["image", "inspect", image],
    };
    Command::new(engine)
        .args(probe_args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Assemble the full argv (engine included at index 0) to run `command` +
/// `child_args` inside `image`. The environment's project directory is
/// bind-mounted at the *same* path it has on the host, so in-container paths
/// match host paths (sidesteps path translation), with cwd set there and
/// `ENWIRO_ENV` injected. `-it` when stdin is a TTY, `-i` otherwise.
#[cfg(feature = "container-wrap")]
fn build_container_argv(
    engine: &str,
    image: &str,
    environment_path: &str,
    environment_name: &str,
    command: &str,
    child_args: &[String],
    interactive: bool,
) -> Vec<String> {
    let mut argv = vec![engine.to_string(), "run".to_string(), "--rm".to_string()];
    argv.push(if interactive { "-it" } else { "-i" }.to_string());
    argv.push("-v".to_string());
    argv.push(format!("{environment_path}:{environment_path}"));
    argv.push("-w".to_string());
    argv.push(environment_path.to_string());
    argv.push("-e".to_string());
    argv.push(format!("ENWIRO_ENV={environment_name}"));
    argv.push(image.to_string());
    argv.push(command.to_string());
    argv.extend(child_args.iter().cloned());
    argv
}

#[cfg(all(test, feature = "container-wrap"))]
mod tests {
    use super::*;

    #[test]
    fn image_tag_is_prefixed_env_name() {
        assert_eq!(container_image_tag("my-proj"), "enwiro/my-proj");
    }

    #[test]
    fn container_argv_mounts_env_path_at_same_path_and_sets_env() {
        let argv = build_container_argv(
            "podman",
            "enwiro/my-proj",
            "/home/u/.enwiro_envs/my-proj/my-proj",
            "my-proj",
            "bash",
            &["-l".to_string()],
            true,
        );
        assert_eq!(
            argv,
            vec![
                "podman",
                "run",
                "--rm",
                "-it",
                "-v",
                "/home/u/.enwiro_envs/my-proj/my-proj:/home/u/.enwiro_envs/my-proj/my-proj",
                "-w",
                "/home/u/.enwiro_envs/my-proj/my-proj",
                "-e",
                "ENWIRO_ENV=my-proj",
                "enwiro/my-proj",
                "bash",
                "-l",
            ]
        );
    }

    #[test]
    fn container_argv_uses_dash_i_when_not_a_tty() {
        let argv = build_container_argv("docker", "enwiro/x", "/p", "x", "echo", &[], false);
        assert!(argv.contains(&"-i".to_string()));
        assert!(!argv.contains(&"-it".to_string()));
    }

    #[test]
    fn container_argv_places_image_before_command_and_args_after() {
        let argv = build_container_argv(
            "podman",
            "enwiro/x",
            "/p",
            "x",
            "just",
            &["build".to_string(), "--release".to_string()],
            false,
        );
        let image_idx = argv.iter().position(|a| a == "enwiro/x").unwrap();
        let cmd_idx = argv.iter().position(|a| a == "just").unwrap();
        assert!(image_idx < cmd_idx, "image must precede the command");
        assert_eq!(&argv[cmd_idx..], &["just", "build", "--release"]);
    }
}
