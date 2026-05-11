use std::process::Command;

use anyhow::{Context, bail};
use clap::Parser;
use enwiro_sdk::{CookbookMetadata, Recipe};

const RECIPE_NAME: &str = "chezmoi";

#[derive(Parser)]
enum EnwiroCookbookChezmoi {
    ListRecipes(ListRecipesArgs),
    Cook(CookArgs),
    Metadata,
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
                    default_priority: Some(20)
                }
                .to_json()
            );
        }
    };

    Ok(())
}
