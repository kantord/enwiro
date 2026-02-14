# enwiro-cookbook-git

A cookbook plugin for [enwiro](https://crates.io/crates/enwiro) that generates environments from local Git repositories.

## Installation

```
cargo install enwiro-cookbook-git
```

## Configuration

Configuration is stored in `~/.config/enwiro/cookbook-git.toml` (managed by [confy](https://crates.io/crates/confy)).

```toml
repo_globs = ["/home/user/projects/*"]
```

The `repo_globs` field is a list of glob patterns that match directories containing Git repositories. Matching repositories will be available as recipes in enwiro.

You can optionally configure where on-demand worktrees are stored:

```toml
repo_globs = ["/home/user/projects/*"]
worktree_dir = "/home/user/.worktrees"
```

If `worktree_dir` is not set, worktrees are stored in `$XDG_DATA_HOME/enwiro/worktrees/` (typically `~/.local/share/enwiro/worktrees/`).

## Recipes

For each discovered repository, the following recipes are generated:

- **`repo-name`** — the repository's main working directory
- **`repo-name@worktree`** — any existing git worktrees
- **`repo-name@branch`** — each local and remote branch that is not currently checked out

Cooking a branch recipe automatically creates a git worktree. The worktree persists after creation, so subsequent cooks reuse the existing worktree.
