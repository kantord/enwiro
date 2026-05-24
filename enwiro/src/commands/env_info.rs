use std::io::Write;

use anyhow::Context;
use clap::Args;
use enwiro_sdk::process::ENWIRO_ENV_VAR;

use crate::context::CommandContext;
use crate::environments::Environment;

#[derive(Args)]
#[command(author, version, about = "Show information about an environment")]
pub struct EnvInfoArgs {
    /// Name of the environment to query. Defaults to the active environment.
    pub name: Option<String>,

    /// Output as JSON. Required; plain text output is not yet implemented.
    #[arg(long)]
    pub json: bool,
}

#[derive(serde::Serialize)]
struct EnvInfoOutput {
    r#type: Option<String>,
    name: Option<String>,
}

fn resolve_env_name_from_daemon() -> Option<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    rt.block_on(async {
        let client = enwiro_sdk::rpc::connect().await.ok()?;
        let result = enwiro_sdk::rpc::EnwiroRpcClient::env_current(&client)
            .await
            .ok()?;
        result.env_name
    })
}

fn resolve_env_name() -> Option<String> {
    if let Some(name) = resolve_env_name_from_daemon() {
        return Some(name);
    }
    std::env::var(ENWIRO_ENV_VAR).ok().filter(|v| !v.is_empty())
}

fn classify_env<W: Write>(ctx: &CommandContext<W>, name: &str) -> Option<String> {
    if Environment::get_one(&ctx.config.workspaces_directory, name).is_ok() {
        return Some("environment".into());
    }
    if ctx.find_recipe_in_cache_by_name(name) {
        return Some("recipe".into());
    }
    None
}

pub fn env_info<W: Write>(ctx: &mut CommandContext<W>, args: EnvInfoArgs) -> anyhow::Result<()> {
    if !args.json {
        anyhow::bail!("Plain text output is not yet implemented. Use --json to get JSON output.");
    }

    let env_name = args.name.or_else(resolve_env_name);
    let env_type = env_name.as_deref().and_then(|name| classify_env(ctx, name));

    let output = EnvInfoOutput {
        r#type: env_type,
        name: env_name,
    };

    let json = serde_json::to_string(&output).context("Failed to serialize output")?;
    write!(ctx.writer, "{json}")?;

    Ok(())
}
