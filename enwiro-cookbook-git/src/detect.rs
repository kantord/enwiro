//! Git-native "is this branch's work already merged into the default
//! branch?" detection, shared by the git and github cookbooks (the latter
//! imports this crate as a library - see #302).
//!
//! Three merge styles, in increasing difficulty:
//! - merge-commit / fast-forward -> ancestry (`graph_descendant_of`)
//! - rebase / cherry-pick        -> per-commit patch-id equivalence
//! - squash                      -> synthetic-squash-commit patch-id,
//!   reliable ONLY while the recorded squash diff still equals the
//!   branch's cumulative diff (fails after the target diverges; that
//!   case is forge-only and handled by the github cookbook).
//!
//! Any uncertainty or error yields [`Verdict::Stray`] - the caller emits
//! no status rather than guessing, so a wrong guess never marks an env
//! `done`.

use std::path::Path;

use git2::{Oid, Repository};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Branch work is present in the default branch (any merge style).
    Merged,
    /// Cannot prove merged or not - caller stays silent.
    Stray,
    /// Branch has unmerged work, or is not a feature branch.
    NotMerged,
}

/// Open the worktree at `worktree_path` and decide whether its current
/// `HEAD` branch is merged into `default_branch` (a local branch name such
/// as `"main"`). Errors and ambiguity collapse to [`Verdict::Stray`].
pub fn detect(worktree_path: &Path, default_branch: &str) -> Verdict {
    match try_detect(worktree_path, default_branch) {
        Ok(v) => v,
        Err(_) => Verdict::Stray,
    }
}

fn try_detect(worktree_path: &Path, default_branch: &str) -> anyhow::Result<Verdict> {
    let repo = Repository::open(worktree_path)?;
    let branch_tip = repo.head()?.peel_to_commit()?.id();
    let default_tip = resolve_branch_tip(&repo, default_branch)?;
    verdict(&repo, branch_tip, default_tip)
}

/// Open the worktree and decide merge status against the repo's *resolved*
/// default branch (`origin/HEAD` -> `main` -> `master`). Errors and
/// ambiguity (including no resolvable default branch) collapse to
/// [`Verdict::Stray`].
pub fn detect_auto(worktree_path: &Path) -> Verdict {
    let Ok(repo) = Repository::open(worktree_path) else {
        return Verdict::Stray;
    };
    let Some(default) = default_branch_name(&repo) else {
        return Verdict::Stray;
    };
    detect(worktree_path, &default)
}

/// Resolve a repo's default branch name: prefer the `origin/HEAD` symbolic
/// target, then a local `main`, then `master`. `None` if none resolve.
pub fn default_branch_name(repo: &Repository) -> Option<String> {
    if let Ok(head) = repo.find_reference("refs/remotes/origin/HEAD")
        && let Some(target) = head.symbolic_target()
    {
        // e.g. "refs/remotes/origin/main" -> "main"
        if let Some(name) = target.rsplit('/').next() {
            return Some(name.to_string());
        }
    }
    for candidate in ["main", "master"] {
        if repo.find_branch(candidate, git2::BranchType::Local).is_ok() {
            return Some(candidate.to_string());
        }
    }
    None
}

fn resolve_branch_tip(repo: &Repository, branch: &str) -> anyhow::Result<Oid> {
    Ok(repo
        .find_branch(branch, git2::BranchType::Local)?
        .into_reference()
        .peel_to_commit()?
        .id())
}

/// Decide merge status of a named branch directly in `repo_path`, without
/// needing a checked-out worktree - used by the git cookbook for branch
/// recipes (whose enwiro-managed worktrees are otherwise invisible). Both
/// the branch tip and the default-branch tip live in the same repo, so a
/// plain [`verdict`] suffices. Errors / no default branch -> [`Verdict::Stray`].
pub fn detect_branch(repo_path: &Path, branch_name: &str, is_remote: bool) -> Verdict {
    match try_detect_branch(repo_path, branch_name, is_remote) {
        Ok(v) => v,
        Err(_) => Verdict::Stray,
    }
}

fn try_detect_branch(
    repo_path: &Path,
    branch_name: &str,
    is_remote: bool,
) -> anyhow::Result<Verdict> {
    let repo = Repository::open(repo_path)?;
    let kind = if is_remote {
        git2::BranchType::Remote
    } else {
        git2::BranchType::Local
    };
    let branch_tip = repo
        .find_branch(branch_name, kind)?
        .into_reference()
        .peel_to_commit()?
        .id();
    let default = default_branch_name(&repo).ok_or_else(|| anyhow::anyhow!("no default branch"))?;
    let default_tip = resolve_branch_tip(&repo, &default)?;
    verdict(&repo, branch_tip, default_tip)
}

/// Rebase / cherry-pick: every feature-side commit's patch already appears on
/// the default branch. An empty feature side never counts as merged.
fn all_feature_patches_present(feature_patch_ids: &[Oid], default_patch_ids: &[Oid]) -> bool {
    !feature_patch_ids.is_empty()
        && feature_patch_ids
            .iter()
            .all(|id| default_patch_ids.contains(id))
}

/// Squash (undiverged only): the cumulative `base..tip` diff collapses to a
/// single patch whose id appears among the default branch's commits.
fn squash_patch_present(
    repo: &Repository,
    base_tree: &git2::Tree,
    tip_tree: &git2::Tree,
    default_patch_ids: &[Oid],
) -> anyhow::Result<bool> {
    let cumulative = patch_id_of_tree_diff(repo, Some(base_tree), tip_tree)?;
    Ok(cumulative.is_some_and(|id| default_patch_ids.contains(&id)))
}

/// Core decision over two commit tips in `repo`. Separated from I/O so it
/// is unit-testable against synthetic repositories. Each merge shape has its
/// own named check; they are ordered cheapest-and-most-definitive first.
pub fn verdict(repo: &Repository, branch_tip: Oid, default_tip: Oid) -> anyhow::Result<Verdict> {
    // The default branch itself, or an identical tip: nothing to merge.
    if branch_tip == default_tip {
        return Ok(Verdict::NotMerged);
    }

    // Merge-commit / fast-forward: branch tip is an ancestor of default. The
    // definitive, net-change-independent signal - checked before the
    // zero-net-change guard (a merge commit makes the branch tip its own
    // merge-base with default, which would otherwise trip that guard).
    if repo.graph_descendant_of(default_tip, branch_tip)? {
        return Ok(Verdict::Merged);
    }

    let base = repo.merge_base(branch_tip, default_tip)?;
    let tip_tree = repo.find_commit(branch_tip)?.tree()?;
    let base_tree = repo.find_commit(base)?.tree()?;

    // Zero-net-change guard: a branch whose tree equals the merge-base tree
    // (e.g. add-then-revert) would spuriously match an empty patch.
    if tip_tree.id() == base_tree.id() {
        return Ok(Verdict::NotMerged);
    }

    // Patch-ids of the commits unique to the default branch since the base -
    // the candidate pool for both the rebase and squash checks.
    let default_patch_ids = patch_ids_in_range(repo, base, default_tip)?;
    let feature_patch_ids = patch_ids_in_range(repo, base, branch_tip)?;

    if all_feature_patches_present(&feature_patch_ids, &default_patch_ids)
        || squash_patch_present(repo, &base_tree, &tip_tree, &default_patch_ids)?
    {
        return Ok(Verdict::Merged);
    }

    Ok(Verdict::Stray)
}

/// Patch-ids of every commit reachable from `tip` but not from `base`.
/// Merge commits (and the root) are diffed against their first parent.
fn patch_ids_in_range(repo: &Repository, base: Oid, tip: Oid) -> anyhow::Result<Vec<Oid>> {
    let mut walk = repo.revwalk()?;
    walk.push(tip)?;
    walk.hide(base)?;
    let mut ids = Vec::new();
    for oid in walk {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        let new_tree = commit.tree()?;
        let parent_tree = match commit.parent(0) {
            Ok(parent) => Some(parent.tree()?),
            Err(_) => None, // root commit: diff against empty tree
        };
        if let Some(id) = patch_id_of_tree_diff(repo, parent_tree.as_ref(), &new_tree)? {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Patch-id of the diff `old_tree -> new_tree`. `None` when the diff is
/// empty (no stable patch-id, and nothing to match).
fn patch_id_of_tree_diff(
    repo: &Repository,
    old_tree: Option<&git2::Tree>,
    new_tree: &git2::Tree,
) -> anyhow::Result<Option<Oid>> {
    let diff = repo.diff_tree_to_tree(old_tree, Some(new_tree), None)?;
    if diff.deltas().len() == 0 {
        return Ok(None);
    }
    Ok(Some(diff.patchid(None)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn sig() -> Signature<'static> {
        Signature::now("T", "t@t.t").unwrap()
    }

    /// Commit `content` to `file` on the current HEAD, return the new commit Oid.
    fn commit_file(repo: &Repository, file: &str, content: &str, msg: &str) -> Oid {
        let root = repo.workdir().unwrap();
        fs::write(root.join(file), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(file)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parents: Vec<git2::Commit> =
            match repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
                Some(c) => vec![c],
                None => vec![],
            };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig(), &sig(), msg, &tree, &parent_refs)
            .unwrap()
    }

    fn init() -> (TempDir, Repository) {
        let dir = TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        // Ensure HEAD points at `main`.
        repo.set_head("refs/heads/main").unwrap();
        (dir, repo)
    }

    fn branch_at(repo: &Repository, name: &str, at: Oid) {
        repo.branch(name, &repo.find_commit(at).unwrap(), true)
            .unwrap();
    }

    #[test]
    fn merge_commit_is_merged() {
        let (_d, repo) = init();
        let base = commit_file(&repo, "a", "1", "base");
        // feature branch adds f
        branch_at(&repo, "feature", base);
        repo.set_head("refs/heads/feature").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let feat = commit_file(&repo, "f", "x", "feat");
        // main advances, then a merge commit brings feature in as 2nd parent
        repo.set_head("refs/heads/main").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let main2 = commit_file(&repo, "a", "2", "main2");
        let merge_tree = repo.find_commit(feat).unwrap().tree().unwrap();
        let merge = repo
            .commit(
                Some("refs/heads/main"),
                &sig(),
                &sig(),
                "merge",
                &merge_tree,
                &[
                    &repo.find_commit(main2).unwrap(),
                    &repo.find_commit(feat).unwrap(),
                ],
            )
            .unwrap();
        assert_eq!(verdict(&repo, feat, merge).unwrap(), Verdict::Merged);
    }

    #[test]
    fn rebase_cherry_pick_is_merged() {
        let (_d, repo) = init();
        let base = commit_file(&repo, "a", "1", "base");
        branch_at(&repo, "feature", base);
        repo.set_head("refs/heads/feature").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let feat = commit_file(&repo, "f", "hello", "add f");
        // Simulate rebase-merge: apply the SAME change onto main as a new commit.
        repo.set_head("refs/heads/main").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let main_with_same = commit_file(&repo, "f", "hello", "add f (replayed)");
        assert_eq!(
            verdict(&repo, feat, main_with_same).unwrap(),
            Verdict::Merged
        );
    }

    #[test]
    fn squash_undiverged_is_merged() {
        let (_d, repo) = init();
        let base = commit_file(&repo, "a", "1", "base");
        branch_at(&repo, "feature", base);
        repo.set_head("refs/heads/feature").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        commit_file(&repo, "f", "line1\n", "feat 1");
        let feat = commit_file(&repo, "f", "line1\nline2\n", "feat 2");
        // Squash: a single commit on main with the cumulative tree of feature.
        repo.set_head("refs/heads/main").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let squash_tree = repo.find_commit(feat).unwrap().tree().unwrap();
        let squash = repo
            .commit(
                Some("refs/heads/main"),
                &sig(),
                &sig(),
                "squash (#1)",
                &squash_tree,
                &[&repo.find_commit(base).unwrap()],
            )
            .unwrap();
        assert_eq!(verdict(&repo, feat, squash).unwrap(), Verdict::Merged);
    }

    #[test]
    fn unmerged_is_not_merged_or_stray() {
        let (_d, repo) = init();
        let base = commit_file(&repo, "a", "1", "base");
        branch_at(&repo, "feature", base);
        repo.set_head("refs/heads/feature").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let feat = commit_file(&repo, "f", "unique work", "feat");
        // main advances on an unrelated file; feature never integrated.
        repo.set_head("refs/heads/main").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let main2 = commit_file(&repo, "b", "other", "main work");
        assert_eq!(verdict(&repo, feat, main2).unwrap(), Verdict::Stray);
    }

    #[test]
    fn detect_branch_finds_merged_branch_without_worktree() {
        let (dir, repo) = init();
        let base = commit_file(&repo, "a", "1", "base");
        branch_at(&repo, "feature", base);
        repo.set_head("refs/heads/feature").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let feat = commit_file(&repo, "f", "x", "feat");
        // Squash-merge feature onto main (single commit, cumulative tree).
        repo.set_head("refs/heads/main").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let squash_tree = repo.find_commit(feat).unwrap().tree().unwrap();
        repo.commit(
            Some("refs/heads/main"),
            &sig(),
            &sig(),
            "squash (#1)",
            &squash_tree,
            &[&repo.find_commit(base).unwrap()],
        )
        .unwrap();
        // Detect by branch name against the bare-ish repo path - no worktree.
        assert_eq!(detect_branch(dir.path(), "feature", false), Verdict::Merged);
        // An unmerged branch name -> Stray (default-branch `main` exists).
        branch_at(&repo, "lonely", base);
        // give `lonely` unique unmerged work
        repo.set_head("refs/heads/lonely").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        commit_file(&repo, "z", "unmerged", "lonely work");
        assert_eq!(detect_branch(dir.path(), "lonely", false), Verdict::Stray);
    }

    #[test]
    fn zero_net_change_is_not_merged() {
        let (_d, repo) = init();
        let base = commit_file(&repo, "a", "1", "base");
        branch_at(&repo, "feature", base);
        repo.set_head("refs/heads/feature").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        commit_file(&repo, "a", "2", "change");
        let feat = commit_file(&repo, "a", "1", "revert"); // tree == base tree
        // main advances separately so feature is not an ancestor of main.
        repo.set_head("refs/heads/main").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        let main2 = commit_file(&repo, "b", "other", "main work");
        // branch_tip tree equals base tree -> NotMerged, never a false Merged.
        assert_eq!(verdict(&repo, feat, main2).unwrap(), Verdict::NotMerged);
    }
}
