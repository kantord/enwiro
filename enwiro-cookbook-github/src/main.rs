use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use clap::Parser;
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ConfigurationValues {
    pub worktree_dir: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RepoConfig {
    pub repo: String,
    pub local_path: PathBuf,
}

/// Minimal representation of the git cookbook's configuration.
/// This intentionally couples to the git cookbook's config schema
/// (confy key "cookbook-git", field "repo_globs"). If the git cookbook
/// renames these, this struct must be updated to match.
#[derive(Debug, Serialize, Deserialize, Default)]
struct GitCookbookConfig {
    repo_globs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PrInfo {
    pub number: u64,
    pub title: String,
    pub head_ref_name: String,
    pub repo: String,
}

#[derive(Parser)]
enum EnwiroCookbookGithub {
    ListRecipes(ListRecipesArgs),
    Cook(CookArgs),
}

#[derive(clap::Args)]
pub struct ListRecipesArgs {}

#[derive(clap::Args)]
pub struct CookArgs {
    recipe_name: String,
}

fn short_path_hash(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(path.to_string_lossy().as_bytes());
    format!("{:x}", hash)[..8].to_string()
}

fn default_worktree_dir() -> anyhow::Result<PathBuf> {
    let base = dirs::data_dir().context("Could not determine data directory (is $HOME set?)")?;
    Ok(base.join("enwiro").join("worktrees").join("pr"))
}

fn worktree_base_dir(config: &ConfigurationValues) -> anyhow::Result<PathBuf> {
    match &config.worktree_dir {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => default_worktree_dir(),
    }
}

/// Parse a GitHub remote URL and extract "owner/repo".
/// Returns None for non-GitHub remotes.
fn parse_github_remote(url: &str) -> Option<String> {
    let url = url.trim();

    // SSH format: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let repo = rest.strip_suffix(".git").unwrap_or(rest);
        return if repo.contains('/') {
            Some(repo.to_string())
        } else {
            None
        };
    }

    // URL formats: https://github.com/..., ssh://git@github.com/..., http://github.com/...
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;

    let repo = path.strip_suffix(".git").unwrap_or(path);
    if repo.contains('/') {
        Some(repo.to_string())
    } else {
        None
    }
}

fn discover_github_repos_from_config(
    git_config: &GitCookbookConfig,
) -> anyhow::Result<Vec<RepoConfig>> {
    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for glob_pattern in &git_config.repo_globs {
        let paths = glob::glob(glob_pattern)
            .with_context(|| format!("Could not parse glob pattern: {}", glob_pattern))?;

        for path in paths.flatten() {
            let repo = match git2::Repository::open(&path) {
                Ok(r) => r,
                Err(_) => continue,
            };

            let origin = match repo.find_remote("origin") {
                Ok(r) => r,
                Err(_) => continue,
            };

            let url = match origin.url() {
                Some(u) => u.to_string(),
                None => continue,
            };

            if let Some(github_repo) = parse_github_remote(&url)
                && seen.insert(github_repo.clone())
            {
                let canonical_path = path.canonicalize().unwrap_or(path);
                tracing::debug!(repo = %github_repo, path = %canonical_path.display(), "Discovered GitHub repo");
                results.push(RepoConfig {
                    repo: github_repo,
                    local_path: canonical_path,
                });
            }
        }
    }

    Ok(results)
}

fn discover_github_repos() -> anyhow::Result<Vec<RepoConfig>> {
    let git_config: GitCookbookConfig = confy::load("enwiro", "cookbook-git")
        .context("Could not load git cookbook configuration")?;
    discover_github_repos_from_config(&git_config)
}

/// Parse a recipe name like "owner/repo#123" into ("owner/repo", 123).
fn parse_recipe_name(name: &str) -> anyhow::Result<(&str, u64)> {
    let (repo, number_str) = name
        .rsplit_once('#')
        .context("Recipe name must contain '#' (expected format: owner/repo#123)")?;
    let number = number_str
        .parse::<u64>()
        .with_context(|| format!("Invalid PR number: {}", number_str))?;
    Ok((repo, number))
}

fn build_search_query(repos: &[String]) -> String {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(30);
    let date_str = cutoff.format("%Y-%m-%d").to_string();
    let repo_filters: Vec<String> = repos.iter().map(|r| format!("repo:{}", r)).collect();
    format!(
        "is:pr is:open {} updated:>{} sort:updated-desc",
        repo_filters.join(" "),
        date_str
    )
}

/// Serde structs for the GraphQL search response.
#[derive(Deserialize)]
struct GraphQlResponse {
    data: GraphQlData,
}

#[derive(Deserialize)]
struct GraphQlData {
    search: GraphQlSearch,
}

#[derive(Deserialize)]
struct GraphQlSearch {
    nodes: Vec<GraphQlPrNode>,
}

#[derive(Deserialize)]
struct GraphQlPrNode {
    number: u64,
    title: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    repository: GraphQlRepo,
}

#[derive(Deserialize)]
struct GraphQlRepo {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

fn parse_search_response(json: &str) -> anyhow::Result<Vec<PrInfo>> {
    let response: GraphQlResponse =
        serde_json::from_str(json).context("Could not parse GraphQL response")?;
    Ok(response
        .data
        .search
        .nodes
        .into_iter()
        .map(|node| PrInfo {
            number: node.number,
            title: node.title,
            head_ref_name: node.head_ref_name,
            repo: node.repository.name_with_owner,
        })
        .collect())
}

const SEARCH_QUERY: &str = r#"query($searchQuery: String!) {
  search(query: $searchQuery, type: ISSUE, first: 100) {
    nodes {
      ... on PullRequest {
        number
        title
        headRefName
        updatedAt
        repository { nameWithOwner }
      }
    }
  }
}"#;

fn search_prs(repos: &[String]) -> anyhow::Result<Vec<PrInfo>> {
    if repos.is_empty() {
        return Ok(Vec::new());
    }

    let search_query = build_search_query(repos);

    let output = Command::new("gh")
        .args([
            "api",
            "graphql",
            "-F",
            &format!("searchQuery={}", search_query),
            "-f",
            &format!("query={}", SEARCH_QUERY),
        ])
        .output()
        .context(
            "Failed to run gh CLI. Is it installed and authenticated? \
             (https://cli.github.com/, then run: gh auth login)",
        )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "gh api graphql failed: {}. Is gh authenticated? (try: gh auth login)",
            stderr
        );
    }

    let stdout = String::from_utf8(output.stdout).context("gh produced invalid UTF-8")?;
    let prs = parse_search_response(&stdout)?;

    if prs.len() >= 100 {
        eprintln!(
            "Warning: GitHub search returned 100 results (the maximum). Some PRs may be missing."
        );
    }

    Ok(prs)
}

fn list_recipes() -> anyhow::Result<()> {
    let repos = discover_github_repos()?;
    let repo_names: Vec<String> = repos.iter().map(|r| r.repo.clone()).collect();

    let prs = search_prs(&repo_names)?;
    for pr in prs {
        let safe_title = pr.title.replace(['\t', '\n', '\0', '\x1f'], " ");
        println!("{}#{}\t{}", pr.repo, pr.number, safe_title);
    }
    Ok(())
}

fn cook(config: &ConfigurationValues, args: CookArgs) -> anyhow::Result<()> {
    let (repo_str, pr_number) = parse_recipe_name(&args.recipe_name)?;

    let repos = discover_github_repos()?;
    let repo_config = repos
        .iter()
        .find(|r| r.repo == repo_str)
        .with_context(|| format!("No configured repo matching '{}'", repo_str))?;

    anyhow::ensure!(
        repo_config.local_path.exists(),
        "Local clone not found at {}. Please clone the repo first.",
        repo_config.local_path.display()
    );

    let wt_base = worktree_base_dir(config)?;
    let path_hash = short_path_hash(&repo_config.local_path);
    let repo_name = repo_str.replace('/', "-");
    let wt_path = wt_base
        .join(format!("{}-{}", repo_name, path_hash))
        .join(format!("pr-{}", pr_number));

    // If worktree already exists, just return the path
    if wt_path.exists() {
        println!(
            "{}",
            wt_path
                .to_str()
                .context("Could not convert worktree path to string")?
        );
        return Ok(());
    }

    // Create parent directories
    std::fs::create_dir_all(wt_path.parent().unwrap())
        .context("Could not create worktree directory")?;

    // Fetch the PR ref
    let local_path_str = repo_config
        .local_path
        .to_str()
        .context("Could not convert local path to string")?;
    let ref_name = format!("pr-{}", pr_number);
    let fetch_refspec = format!("pull/{}/head:{}", pr_number, ref_name);
    let fetch_status = Command::new("git")
        .args(["-C", local_path_str, "fetch", "origin", &fetch_refspec])
        .status()
        .context("Failed to run git fetch")?;

    if !fetch_status.success() {
        anyhow::bail!(
            "Failed to fetch PR #{} from {}",
            pr_number,
            repo_config.repo
        );
    }

    // Create worktree using git2
    let repo = git2::Repository::open(&repo_config.local_path)
        .context("Could not open repository for worktree creation")?;
    let branch = repo
        .find_branch(&ref_name, git2::BranchType::Local)
        .with_context(|| format!("Could not find branch {}", ref_name))?;
    let reference = branch.into_reference();

    let wt_name = format!("enwiro-pr-{}", pr_number);
    let mut opts = git2::WorktreeAddOptions::new();
    opts.reference(Some(&reference));
    repo.worktree(&wt_name, &wt_path, Some(&opts))
        .with_context(|| format!("Could not create worktree for PR #{}", pr_number))?;

    tracing::debug!(path = %wt_path.display(), pr = pr_number, "Created worktree for PR");
    println!(
        "{}",
        wt_path
            .to_str()
            .context("Could not convert worktree path to string")?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_recipe_name_valid() {
        let (repo, number) = parse_recipe_name("kantord/enwiro#42").unwrap();
        assert_eq!(repo, "kantord/enwiro");
        assert_eq!(number, 42);
    }

    #[test]
    fn test_parse_recipe_name_large_number() {
        let (repo, number) = parse_recipe_name("vercel/next.js#12345").unwrap();
        assert_eq!(repo, "vercel/next.js");
        assert_eq!(number, 12345);
    }

    #[test]
    fn test_parse_recipe_name_no_hash() {
        let result = parse_recipe_name("kantord/enwiro");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_recipe_name_invalid_number() {
        let result = parse_recipe_name("kantord/enwiro#abc");
        assert!(result.is_err());
    }

    #[test]
    fn test_build_search_query_single_repo() {
        let repos = vec!["kantord/enwiro".to_string()];
        let query = build_search_query(&repos);
        assert!(query.contains("repo:kantord/enwiro"));
        assert!(query.contains("is:pr"));
        assert!(query.contains("is:open"));
        assert!(query.contains("sort:updated-desc"));
    }

    #[test]
    fn test_build_search_query_multiple_repos() {
        let repos = vec![
            "kantord/enwiro".to_string(),
            "expressjs/express".to_string(),
        ];
        let query = build_search_query(&repos);
        assert!(query.contains("repo:kantord/enwiro"));
        assert!(query.contains("repo:expressjs/express"));
    }

    #[test]
    fn test_build_search_query_includes_date_filter() {
        let repos = vec!["kantord/enwiro".to_string()];
        let query = build_search_query(&repos);
        assert!(
            query.contains("updated:>"),
            "Should contain date filter, got: {}",
            query
        );
    }

    #[test]
    fn test_parse_graphql_response() {
        let json = r#"{
            "data": {
                "search": {
                    "nodes": [
                        {
                            "number": 42,
                            "title": "Fix the thing",
                            "headRefName": "fix-thing",
                            "updatedAt": "2026-02-14T13:10:29Z",
                            "repository": { "nameWithOwner": "kantord/enwiro" }
                        },
                        {
                            "number": 99,
                            "title": "Add feature",
                            "headRefName": "feature/add-stuff",
                            "updatedAt": "2026-02-13T10:00:00Z",
                            "repository": { "nameWithOwner": "expressjs/express" }
                        }
                    ]
                }
            }
        }"#;
        let prs = parse_search_response(json).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].number, 42);
        assert_eq!(prs[0].title, "Fix the thing");
        assert_eq!(prs[0].head_ref_name, "fix-thing");
        assert_eq!(prs[0].repo, "kantord/enwiro");
        assert_eq!(prs[1].number, 99);
        assert_eq!(prs[1].repo, "expressjs/express");
    }

    #[test]
    fn test_parse_graphql_response_empty_nodes() {
        let json = r#"{"data": {"search": {"nodes": []}}}"#;
        let prs = parse_search_response(json).unwrap();
        assert!(prs.is_empty());
    }

    #[test]
    fn test_parse_github_remote_ssh() {
        assert_eq!(
            parse_github_remote("git@github.com:kantord/enwiro.git"),
            Some("kantord/enwiro".to_string())
        );
    }

    #[test]
    fn test_parse_github_remote_https_with_git_suffix() {
        assert_eq!(
            parse_github_remote("https://github.com/kantord/enwiro.git"),
            Some("kantord/enwiro".to_string())
        );
    }

    #[test]
    fn test_parse_github_remote_https_without_git_suffix() {
        assert_eq!(
            parse_github_remote("https://github.com/kantord/enwiro"),
            Some("kantord/enwiro".to_string())
        );
    }

    #[test]
    fn test_parse_github_remote_ssh_protocol() {
        assert_eq!(
            parse_github_remote("ssh://git@github.com/kantord/enwiro.git"),
            Some("kantord/enwiro".to_string())
        );
    }

    #[test]
    fn test_parse_github_remote_gitlab_returns_none() {
        assert_eq!(
            parse_github_remote("git@gitlab.com:kantord/project.git"),
            None
        );
    }

    #[test]
    fn test_parse_github_remote_empty_string() {
        assert_eq!(parse_github_remote(""), None);
    }

    #[test]
    fn test_discover_finds_github_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("enwiro");
        std::fs::create_dir(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        repo.remote("origin", "git@github.com:kantord/enwiro.git")
            .unwrap();

        let git_config = GitCookbookConfig {
            repo_globs: vec![tmp.path().join("*").to_str().unwrap().to_string()],
        };

        let repos = discover_github_repos_from_config(&git_config).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].repo, "kantord/enwiro");
        assert_eq!(repos[0].local_path, repo_path.canonicalize().unwrap());
    }

    #[test]
    fn test_discover_skips_non_github_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("project");
        std::fs::create_dir(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        repo.remote("origin", "git@gitlab.com:kantord/project.git")
            .unwrap();

        let git_config = GitCookbookConfig {
            repo_globs: vec![tmp.path().join("*").to_str().unwrap().to_string()],
        };

        let repos = discover_github_repos_from_config(&git_config).unwrap();
        assert_eq!(repos.len(), 0);
    }

    #[test]
    fn test_discover_skips_repo_without_origin() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("project");
        std::fs::create_dir(&repo_path).unwrap();
        git2::Repository::init(&repo_path).unwrap();

        let git_config = GitCookbookConfig {
            repo_globs: vec![tmp.path().join("*").to_str().unwrap().to_string()],
        };

        let repos = discover_github_repos_from_config(&git_config).unwrap();
        assert_eq!(repos.len(), 0);
    }

    #[test]
    fn test_discover_skips_non_repo_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let not_a_repo = tmp.path().join("just-a-folder");
        std::fs::create_dir(&not_a_repo).unwrap();

        let git_config = GitCookbookConfig {
            repo_globs: vec![tmp.path().join("*").to_str().unwrap().to_string()],
        };

        let repos = discover_github_repos_from_config(&git_config).unwrap();
        assert_eq!(repos.len(), 0);
    }

    #[test]
    fn test_discover_deduplicates_repos() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("enwiro");
        std::fs::create_dir(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        repo.remote("origin", "git@github.com:kantord/enwiro.git")
            .unwrap();

        let git_config = GitCookbookConfig {
            repo_globs: vec![
                tmp.path().join("*").to_str().unwrap().to_string(),
                repo_path.to_str().unwrap().to_string(),
            ],
        };

        let repos = discover_github_repos_from_config(&git_config).unwrap();
        assert_eq!(repos.len(), 1);
    }

    #[test]
    fn test_default_worktree_dir_is_absolute() {
        let dir = default_worktree_dir().unwrap();
        assert!(
            dir.is_absolute(),
            "default_worktree_dir should return an absolute path, got: {:?}",
            dir
        );
    }

    #[test]
    fn test_worktree_base_dir_uses_config() {
        let config = ConfigurationValues {
            worktree_dir: Some("/tmp/my-pr-worktrees".to_string()),
            ..Default::default()
        };
        let dir = worktree_base_dir(&config).unwrap();
        assert_eq!(dir, PathBuf::from("/tmp/my-pr-worktrees"));
    }

    #[test]
    fn test_cook_creates_worktree_for_pr() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        std::fs::create_dir(&repo_path).unwrap();

        // Create a repo with an initial commit
        let repo = git2::Repository::init(&repo_path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // Create a branch simulating a fetched PR ref
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("pr-42", &head, false).unwrap();

        let wt_dir = tmp.path().join("worktrees");
        let repo_config = RepoConfig {
            repo: "kantord/enwiro".to_string(),
            local_path: repo_path.clone(),
        };

        // Simulate what cook does after the fetch step
        let path_hash = short_path_hash(&repo_config.local_path);
        let wt_path = wt_dir
            .join(format!("kantord-enwiro-{}", path_hash))
            .join("pr-42");

        std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();

        let branch = repo.find_branch("pr-42", git2::BranchType::Local).unwrap();
        let reference = branch.into_reference();
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(&reference));
        repo.worktree("enwiro-pr-42", &wt_path, Some(&opts))
            .unwrap();

        assert!(wt_path.exists(), "Worktree path should exist on disk");
        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        assert!(wt_repo.is_worktree(), "Should be a git worktree");
    }
}

fn main() -> anyhow::Result<()> {
    let _guard = enwiro_logging::init_logging("enwiro-cookbook-github.log");

    let args = EnwiroCookbookGithub::parse();
    let config: ConfigurationValues =
        confy::load("enwiro", "cookbook-github").context("Could not load configuration")?;
    tracing::debug!("Config loaded, repos will be auto-discovered from git cookbook");

    match args {
        EnwiroCookbookGithub::ListRecipes(_) => {
            list_recipes()?;
        }
        EnwiroCookbookGithub::Cook(args) => {
            cook(&config, args)?;
        }
    };

    Ok(())
}
