//! Clap building blocks for Rust plugin binaries (feature `cli`).
//!
//! Each plugin kind's *required* subcommands ship as one [`clap::Subcommand`]
//! enum that a plugin adopts wholesale with `#[command(flatten)]`:
//!
//! ```ignore
//! #[derive(clap::Parser)]
//! enum EnwiroCookbookGit {
//!     #[command(flatten)]
//!     Core(CookbookCore),
//!     Listen,                          // optional capability, opted into
//!     ExternalPaths(ExternalPathsArgs) // plugin-specific extras stay legal
//! }
//! ```
//!
//! This makes the kind's base contract a compile-time guarantee twice over:
//! the required set is atomic (you cannot adopt half of a core enum), and
//! Rust's exhaustive `match` then forces a handler for every core variant -
//! including `Metadata`, so a plugin built on a core enum can never lack
//! the probe the host relies on. Optional capabilities are the plugin's own
//! variants, declared to the host via the kind's metadata type (see
//! [`crate::metadata`]).

// Required subcommands of every cookbook: `list-recipes`, `cook`, and
// `metadata`. Optional surface (`listen`, `external-paths`, `gear`) is
// added per-plugin. A regular comment, not a doc comment: clap derives
// would otherwise surface it as the adopting plugin's own --help about.
#[derive(Debug, clap::Subcommand)]
pub enum CookbookCore {
    /// Print the cookbook's recipes as JSONL on stdout.
    ListRecipes(ListRecipesArgs),
    /// Materialize a recipe and print the resulting environment path.
    Cook(CookArgs),
    /// Print the cookbook's metadata (capabilities, priority, ...) as JSON.
    Metadata,
}

#[derive(Debug, clap::Args)]
pub struct ListRecipesArgs {}

#[derive(Debug, clap::Args)]
pub struct CookArgs {
    pub recipe_name: String,
}

// Required subcommands of every adapter: `get-active-workspace-id`,
// `activate`, `run`, and `metadata`. Optional surface (`listen`) is added
// per-plugin. Regular comment for the same clap --help reason as above.
#[derive(Debug, clap::Subcommand)]
pub enum AdapterCore {
    /// Print the identifier of the currently focused workspace.
    GetActiveWorkspaceId(GetActiveWorkspaceIdArgs),
    /// Switch to the named environment's workspace.
    Activate(ActivateArgs),
    /// Spawn a command in the adapter's native context (RunPayload on stdin).
    Run(RunArgs),
    /// Print the adapter's metadata (capabilities) as JSON.
    Metadata,
}

#[derive(Debug, clap::Args)]
pub struct GetActiveWorkspaceIdArgs {}

#[derive(Debug, clap::Args)]
pub struct ActivateArgs {
    pub name: String,
}

#[derive(Debug, clap::Args)]
pub struct RunArgs {}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    enum FixturePlugin {
        #[command(flatten)]
        Core(CookbookCore),
        Listen,
    }

    #[test]
    fn core_subcommands_flatten_into_a_plugin_cli() {
        let parsed = FixturePlugin::try_parse_from(["fixture", "cook", "my-recipe"]).unwrap();
        let FixturePlugin::Core(CookbookCore::Cook(args)) = parsed else {
            panic!("expected the flattened cook subcommand");
        };
        assert_eq!(args.recipe_name, "my-recipe");
    }

    #[test]
    fn metadata_subcommand_is_part_of_the_core() {
        let parsed = FixturePlugin::try_parse_from(["fixture", "metadata"]).unwrap();
        assert!(matches!(
            parsed,
            FixturePlugin::Core(CookbookCore::Metadata)
        ));
    }

    #[test]
    fn plugin_specific_subcommands_stay_legal_next_to_the_core() {
        let parsed = FixturePlugin::try_parse_from(["fixture", "listen"]).unwrap();
        assert!(matches!(parsed, FixturePlugin::Listen));
    }

    #[derive(Parser)]
    enum FixtureAdapter {
        #[command(flatten)]
        Core(AdapterCore),
    }

    #[test]
    fn adapter_core_covers_the_required_surface() {
        for argv in [
            vec!["fixture", "get-active-workspace-id"],
            vec!["fixture", "activate", "my-env"],
            vec!["fixture", "run"],
            vec!["fixture", "metadata"],
        ] {
            assert!(
                FixtureAdapter::try_parse_from(&argv).is_ok(),
                "expected {argv:?} to parse"
            );
        }
    }
}
