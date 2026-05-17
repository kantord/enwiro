//! `enwiro-garnish-submodules` — for any git project with submodules,
//! contributes a single `cli` gear entry that runs `git submodule update
//! --init --recursive`. The gear is marked `run-on: ["cook"]` so the
//! daemon fires the entry once when the env is cooked. Discovered by
//! `enwiro` via the standard `PluginKind::Garnish` mechanism.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use enwiro_sdk::gear::{CliEntry, Gear, GearFileData, Hook, SCHEMA_VERSION};

const GEAR_NAME: &str = "init-submodules";
const GEAR_DESCRIPTION: &str = "Initialise git submodules";
const ENTRY_NAME: &str = "update";
const ENTRY_DESCRIPTION: &str = "Initialise and update all submodules";

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Exits 0 when the project at `project_dir` is a git repo with at
    /// least one submodule configured. Any other case (not a git repo,
    /// bare repo, no submodules) exits 1.
    AppliesTo { project_dir: PathBuf },
    /// Emit `GearFileData` JSON describing the init-submodules gear.
    Gear { project_dir: PathBuf },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Cmd::AppliesTo { project_dir } => {
            if applies_to(&project_dir) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Cmd::Gear { project_dir: _ } => {
            serde_json::to_writer(std::io::stdout(), &build_gear()).unwrap();
            ExitCode::SUCCESS
        }
    }
}

/// Returns true when `project_dir` is a git repo with at least one
/// submodule. Bare repos, non-git directories, and repos without
/// submodules all return false.
fn applies_to(project_dir: &Path) -> bool {
    let Ok(repo) = git2::Repository::open(project_dir) else {
        return false;
    };
    repo.submodules().map(|s| !s.is_empty()).unwrap_or(false)
}

fn build_gear() -> GearFileData {
    let cli = HashMap::from([(
        ENTRY_NAME.to_owned(),
        CliEntry {
            description: Some(ENTRY_DESCRIPTION.into()),
            command: vec![
                "git".into(),
                "submodule".into(),
                "update".into(),
                "--init".into(),
                "--recursive".into(),
            ],
            run_on: vec![Hook::Cook],
            // Autorun implies the producer vouches for the command.
            require_confirmation: false,
        },
    )]);
    let gear = HashMap::from([(
        GEAR_NAME.to_owned(),
        Gear {
            description: GEAR_DESCRIPTION.into(),
            cli,
            ..Default::default()
        },
    )]);
    GearFileData {
        version: SCHEMA_VERSION,
        gear,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn init_repo(dir: &Path) {
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir)
            .status()
            .expect("run git init");
        assert!(status.success(), "git init must succeed");
    }

    /// Create an empty commit so the repo has a HEAD. `git submodule add`
    /// requires this on the parent, and a parent-less submodule origin
    /// also needs at least one commit to be cloneable.
    fn commit_empty(dir: &Path) {
        let status = Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--allow-empty",
                "-m",
                "init",
                "-q",
            ])
            .current_dir(dir)
            .status()
            .expect("run git commit");
        assert!(status.success(), "git commit must succeed");
    }

    /// `git submodule add` + commit. The commit is intentional: tests that
    /// exercise worktrees need the submodule's `.gitmodules` to be in HEAD,
    /// otherwise `git worktree add` creates a worktree whose HEAD predates
    /// the submodule and `Repository::submodules()` correctly reports none.
    fn add_submodule(parent: &Path, sub_url: &str, sub_path: &str) {
        let status = Command::new("git")
            .args([
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                sub_url,
                sub_path,
            ])
            .current_dir(parent)
            .status()
            .expect("run git submodule add");
        assert!(status.success(), "git submodule add must succeed");
        let status = Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "add submodule",
            ])
            .current_dir(parent)
            .status()
            .expect("run git commit (submodule)");
        assert!(status.success(), "git commit (submodule) must succeed");
    }

    #[test]
    fn applies_false_for_non_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!applies_to(dir.path()));
    }

    #[test]
    fn applies_false_for_git_repo_without_submodules() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        assert!(!applies_to(dir.path()));
    }

    #[test]
    fn applies_true_for_git_repo_with_a_submodule() {
        let outer = tempfile::tempdir().unwrap();
        let sub_origin = outer.path().join("sub-origin");
        fs::create_dir(&sub_origin).unwrap();
        init_repo(&sub_origin);
        commit_empty(&sub_origin);

        let parent = outer.path().join("parent");
        fs::create_dir(&parent).unwrap();
        init_repo(&parent);
        commit_empty(&parent);

        add_submodule(&parent, sub_origin.to_str().unwrap(), "vendor/sub");
        assert!(applies_to(&parent));
    }

    /// The git cookbook cooks branch recipes into linked worktrees, so the
    /// path the garnish receives is often a worktree path rather than the
    /// parent repo's workdir. `Repository::open` handles this transparently
    /// — `submodules()` reports the worktree's HEAD's `.gitmodules`.
    #[test]
    fn applies_correctly_to_a_linked_worktree() {
        let outer = tempfile::tempdir().unwrap();
        let sub_origin = outer.path().join("sub-origin");
        fs::create_dir(&sub_origin).unwrap();
        init_repo(&sub_origin);
        commit_empty(&sub_origin);

        let parent = outer.path().join("parent");
        fs::create_dir(&parent).unwrap();
        init_repo(&parent);
        commit_empty(&parent);
        add_submodule(&parent, sub_origin.to_str().unwrap(), "vendor/sub");

        let wt_path = outer.path().join("parent-worktree");
        let status = Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "worktree",
                "add",
                "-q",
                wt_path.to_str().unwrap(),
            ])
            .current_dir(&parent)
            .status()
            .expect("run git worktree add");
        assert!(status.success(), "git worktree add must succeed");
        assert!(
            applies_to(&wt_path),
            "applies_to must return true for a linked worktree of a repo with submodules"
        );
    }

    #[test]
    fn gear_has_expected_structure() {
        let data = build_gear();
        assert_eq!(data.version, SCHEMA_VERSION);
        let gear = data
            .gear
            .get(GEAR_NAME)
            .expect("init-submodules gear must be present");
        assert_eq!(gear.description, GEAR_DESCRIPTION);
        let entry = gear
            .cli
            .get(ENTRY_NAME)
            .expect("update cli entry must be present");
        assert_eq!(
            entry.command,
            vec!["git", "submodule", "update", "--init", "--recursive"]
        );
        assert_eq!(entry.run_on, vec![Hook::Cook]);
    }
}
