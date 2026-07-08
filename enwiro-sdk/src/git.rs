//! Shared git2 helpers for cookbooks (behind the `git` feature).
//!
//! Single source of truth for "what is this repo's remote default branch" —
//! the git and github cookbooks both fork new branches from it and must
//! agree on how it is resolved.

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
}
