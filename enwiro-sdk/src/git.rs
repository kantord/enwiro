//! Shared git helpers for cookbooks (behind the `git` feature) - most via
//! git2, `remove_worktree` by shelling out to the `git` binary instead
//! (see its own doc comment for why).
//!
//! Single source of truth for "what is this repo's remote default branch" -
//! the git and github cookbooks both fork new branches from it and must
//! agree on how it is resolved. Also owns worktree removal, for the same
//! reason: both cookbooks manage worktrees and must tear them down the
//! same safe way.

use std::path::Path;

use crate::cookbook::PruneOutcome;

/// Remove a worktree via `git worktree remove`, run from inside its base
/// repository. Deliberately shells out to the real `git` binary rather
/// than using git2's lower-level `Worktree::prune` - only the CLI command
/// implements the "refuse if the worktree has uncommitted changes" safety
/// check this needs, and reimplementing that check risks missing an edge
/// case the CLI already handles correctly.
///
/// Reports the outcome as a [`PruneOutcome`]: removed, or kept with a
/// one-line, human-readable reason (see [`kept_reason`]).
pub fn remove_worktree(repo_path: &Path, worktree_path: &Path) -> std::io::Result<PruneOutcome> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("worktree")
        .arg("remove")
        .arg(worktree_path)
        .output()?;

    Ok(if output.status.success() {
        PruneOutcome::Removed
    } else {
        PruneOutcome::Kept {
            reason: kept_reason(
                repo_path,
                worktree_path,
                &String::from_utf8_lossy(&output.stderr),
            ),
        }
    })
}

/// One actionable line out of `git worktree remove`'s stderr, which is
/// noisy (it repeats the path, and can run to several lines). Known shapes
/// get a manual fix the user can paste; anything else keeps git's first
/// line, so an unexpected failure is still reported rather than hidden.
fn kept_reason(repo_path: &Path, worktree_path: &Path, stderr: &str) -> String {
    let first_line = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let message = first_line
        .strip_prefix("fatal: ")
        .or_else(|| first_line.strip_prefix("error: "))
        .unwrap_or(first_line);
    let path = worktree_path.display().to_string();
    let repo = repo_path.display();

    if message.contains("contains modified or untracked files") {
        format!(
            "uncommitted changes in {path} - commit or discard them, or run: \
             git -C '{repo}' worktree remove --force '{path}'"
        )
    } else if message.contains("locked working tree") {
        format!("{path} is locked - unlock it with: git -C '{repo}' worktree unlock '{path}'")
    } else if message.is_empty() {
        format!("git could not remove {path}")
    } else if message.contains(&path) {
        message.to_string()
    } else {
        format!("{path}: {message}")
    }
}

/// The remote default branch name: `origin/HEAD`'s target, else probe
/// `origin/main` and `origin/master`. `None` when the repo has no remote
/// default; what to do then is the caller's policy (the git cookbook falls
/// back to local HEAD, the github cookbook errors).
pub fn remote_default_branch(repo: &git2::Repository) -> Option<String> {
    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD")
        && let Ok(resolved) = reference.resolve()
        && let Ok(name) = resolved.shorthand()
    {
        return Some(name.strip_prefix("origin/").unwrap_or(name).to_string());
    }

    tracing::debug!("origin/HEAD is not set, probing for default branch");
    ["main", "master"]
        .iter()
        .find(|candidate| {
            repo.find_reference(&format!("refs/remotes/origin/{}", candidate))
                .is_ok()
        })
        .map(|name| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_with_commit(path: &std::path::Path) -> git2::Repository {
        let repo = git2::Repository::init(path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        {
            let tree_id = repo.index().unwrap().write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        repo
    }

    #[test]
    fn resolves_origin_head_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let remote_path = tmp.path().join("remote");
        repo_with_commit(&remote_path);
        let local =
            git2::Repository::clone(remote_path.to_str().unwrap(), tmp.path().join("local"))
                .unwrap();

        let default_branch = remote_default_branch(&local).unwrap();
        assert!(
            ["main", "master"].contains(&default_branch.as_str()),
            "got: {default_branch}"
        );
    }

    #[test]
    fn none_for_repo_without_remote() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = repo_with_commit(tmp.path());
        assert_eq!(remote_default_branch(&repo), None);
    }

    fn add_worktree(repo: &git2::Repository, branch_name: &str, wt_path: &std::path::Path) {
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let branch = repo.branch(branch_name, &commit, false).unwrap();
        let reference = branch.into_reference();
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(&reference));
        repo.worktree("test-wt", wt_path, Some(&opts)).unwrap();
    }

    #[test]
    fn removes_a_clean_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        let repo = repo_with_commit(&repo_path);
        let wt_path = tmp.path().join("wt");
        add_worktree(&repo, "feature", &wt_path);
        assert!(wt_path.exists());

        let outcome = remove_worktree(&repo_path, &wt_path).unwrap();
        assert_eq!(outcome, PruneOutcome::Removed);
        assert!(!wt_path.exists(), "worktree directory must be gone");
    }

    #[test]
    fn keeps_a_dirty_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        let repo = repo_with_commit(&repo_path);
        let wt_path = tmp.path().join("wt");
        add_worktree(&repo, "feature", &wt_path);
        std::fs::write(wt_path.join("dirty.txt"), b"uncommitted").unwrap();

        let outcome = remove_worktree(&repo_path, &wt_path).unwrap();
        let PruneOutcome::Kept { reason } = outcome else {
            panic!("a dirty worktree must be kept, got: {outcome:?}");
        };
        assert!(reason.contains("uncommitted changes"), "got: {reason}");
        assert!(
            reason.contains("--force"),
            "must hint the fix, got: {reason}"
        );
        assert!(wt_path.exists(), "dirty worktree must survive");
    }

    const REPO: &str = "/home/u/repo";
    const WT: &str = "/home/u/wt/feature";

    fn reason_for(stderr: &str) -> String {
        kept_reason(Path::new(REPO), Path::new(WT), stderr)
    }

    #[test]
    fn kept_reason_explains_dirty_worktree_with_a_manual_fix() {
        let reason = reason_for(&format!(
            "fatal: '{WT}' contains modified or untracked files, use --force to delete it\n"
        ));
        assert_eq!(
            reason,
            format!(
                "uncommitted changes in {WT} - commit or discard them, or run: \
                 git -C '{REPO}' worktree remove --force '{WT}'"
            )
        );
    }

    #[test]
    fn kept_reason_keeps_git_text_for_permission_denied() {
        let reason = reason_for(&format!(
            "error: failed to delete '{WT}': Permission denied\nsecond line noise\n"
        ));
        assert_eq!(
            reason,
            format!("failed to delete '{WT}': Permission denied")
        );
    }

    #[test]
    fn kept_reason_explains_locked_worktree() {
        let reason = reason_for(
            "fatal: cannot remove a locked working tree, lock reason: usb\n\
             use 'remove -f -f' to override or unlock first\n",
        );
        assert_eq!(
            reason,
            format!("{WT} is locked - unlock it with: git -C '{REPO}' worktree unlock '{WT}'")
        );
    }

    #[test]
    fn kept_reason_names_the_path_for_unrecognized_or_empty_stderr() {
        assert_eq!(
            reason_for("fatal: something new\n"),
            format!("{WT}: something new")
        );
        assert_eq!(reason_for(""), format!("git could not remove {WT}"));
    }
}
