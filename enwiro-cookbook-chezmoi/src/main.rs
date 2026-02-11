use std::process::Command;

use anyhow::{Context, bail};
use clap::Parser;

const RECIPE_NAME: &str = "chezmoi";

#[derive(Parser)]
enum EnwiroCookbookChezmoi {
    ListRecipes(ListRecipesArgs),
    Cook(CookArgs),
}

#[derive(clap::Args)]
pub struct ListRecipesArgs {}

#[derive(clap::Args)]
pub struct CookArgs {
    recipe_name: String,
}

fn list_recipes() {
    println!("{}", RECIPE_NAME);
}

fn cook(args: CookArgs) -> anyhow::Result<()> {
    if args.recipe_name != RECIPE_NAME {
        bail!("Unknown recipe: {}", args.recipe_name);
    }

    let output = Command::new("chezmoi")
        .arg("source-path")
        .output()
        .context("Failed to run chezmoi. Is it installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("chezmoi source-path failed: {}", stderr);
    }

    let path = String::from_utf8(output.stdout).context("chezmoi produced invalid UTF-8 output")?;
    print!("{}", path.trim());

    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = EnwiroCookbookChezmoi::parse();

    match args {
        EnwiroCookbookChezmoi::ListRecipes(_) => {
            list_recipes();
        }
        EnwiroCookbookChezmoi::Cook(args) => {
            cook(args)?;
        }
    };

    Ok(())
}
