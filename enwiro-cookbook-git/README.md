# enwiro-cookbook-git

A cookbook plugin for [enwiro](https://crates.io/crates/enwiro) that generates environments from local Git repositories.

## Installation

```
cargo install enwiro-cookbook-git
```

## Configuration

Configuration is stored in `enwiro/cookbook-git.toml` (managed by [confy](https://crates.io/crates/confy)).

```toml
repo_globs = ["/home/user/projects/*"]
```

The `repo_globs` field is a list of glob patterns that match directories containing Git repositories. Matching repositories will be available as recipes in enwiro.
