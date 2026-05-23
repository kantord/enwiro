use std::process::Command;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Parser;
use enwiro_sdk::{CookbookMetadata, CookbookPayload, Recipe};

const RECIPE_NAME: &str = "chezmoi";
const LISTEN_POLL_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Parser)]
enum EnwiroCookbookChezmoi {
    ListRecipes(ListRecipesArgs),
    Cook(CookArgs),
    Metadata,
    Listen,
}

#[derive(clap::Args)]
pub struct ListRecipesArgs {}

#[derive(clap::Args)]
pub struct CookArgs {
    recipe_name: String,
}

fn list_recipes() {
    println!("{}", Recipe::new(RECIPE_NAME).to_jsonl());
}

fn cook(args: CookArgs) -> anyhow::Result<()> {
    if args.recipe_name != RECIPE_NAME {
        tracing::error!(recipe = %args.recipe_name, "Unknown recipe requested");
        bail!("Unknown recipe: {}", args.recipe_name);
    }

    tracing::debug!("Executing chezmoi source-path");
    let output = Command::new("chezmoi")
        .arg("source-path")
        .output()
        .context("Failed to run chezmoi. Is it installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(%stderr, "chezmoi source-path failed");
        bail!("chezmoi source-path failed: {}", stderr);
    }

    let path = String::from_utf8(output.stdout).context("chezmoi produced invalid UTF-8 output")?;
    tracing::debug!(path = %path.trim(), "Resolved chezmoi source path");
    print!("{}", path.trim());

    Ok(())
}

fn main() -> anyhow::Result<()> {
    let _guard = enwiro_sdk::init_logging("enwiro-cookbook-chezmoi.log");

    let args = EnwiroCookbookChezmoi::parse();

    match args {
        EnwiroCookbookChezmoi::ListRecipes(_) => {
            list_recipes();
        }
        EnwiroCookbookChezmoi::Cook(args) => {
            cook(args)?;
        }
        EnwiroCookbookChezmoi::Metadata => {
            println!(
                "{}",
                CookbookMetadata {
                    default_priority: Some(20),
                    project_overridable: vec![],
                }
                .to_json()
            );
        }
        EnwiroCookbookChezmoi::Listen => {
            let _ = CookbookPayload::read_first_line_from_stdin()
                .context("Could not read cookbook payload from stdin")?;
            enwiro_sdk::listen::serve(LISTEN_POLL_INTERVAL, || vec![Recipe::new(RECIPE_NAME)]);
        }
    };

    Ok(())
}
