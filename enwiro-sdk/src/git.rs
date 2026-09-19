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

/// Remove a worktree via `git worktree remove`, run from inside its base
/// repository. Deliberately shells out to the real `git` binary rather
/// than using git2's lower-level `Worktree::prune` - only the CLI command
/// implements the "refuse if the worktree has uncommitted changes" safety
/// check this needs, and reimplementing that check risks missing an edge
/// case the CLI already handles correctly.
///
/// Returns a short, human-readable outcome either way - removed, or kept
/// with the reason (e.g. uncommitted changes) - for the caller to surface
/// verbatim; it never needs to parse this.
pub fn remove_worktree(repo_path: &Path, worktree_path: &Path) -> std::io::Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("worktree")
        .arg("remove")
        .arg(worktree_path)
        .output()?;

    Ok(if output.status.success() {
        format!("removed worktree at {}", worktree_path.display())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        format!(
            "kept worktree at {} ({})",
            worktree_path.display(),
            stderr.trim()
        )
    })
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
        assert!(outcome.contains("removed worktree"), "got: {outcome}");
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
        assert!(outcome.contains("kept worktree"), "got: {outcome}");
        assert!(wt_path.exists(), "dirty worktree must survive");
    }
}
