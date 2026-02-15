# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Development Setup

See `CONTRIBUTING.md` for full development environment setup, including how to switch between local dev builds and crates.io releases using `just install-dev` / `just install-release`.

## Build & Test Commands

```bash
cargo build --workspace                    # Build all crates
cargo test --workspace                     # Run all tests
cargo test -p enwiro                       # Run tests for a specific crate
cargo test -p enwiro test_cook_environment # Run a single test by name
cargo clippy --workspace -- -D warnings    # Lint (CI runs with -D warnings)
cargo fmt --all --check                    # Check formatting
cargo install --path enwiro-cookbook-git    # Install a crate locally for manual testing
```

macOS CI only builds: `enwiro`, `enwiro-cookbook-git`, `enwiro-bridge-rofi` (no i3wm adapter).

## Architecture

Enwiro connects window manager workspaces to project environments. An environment is a directory in `~/.enwiro_envs/` containing a same-named symlink pointing to the working directory, plus a `meta.json` file for colocated metadata:

```
~/.enwiro_envs/
  my-project/
    my-project → /home/user/code/my-project   # symlink (named to match env for shell status bars)
    meta.json                                  # per-env metadata (frecency stats, description, cookbook)
```

Legacy bare symlinks (pre-directory format) are still discovered for backward compatibility. The inner symlink uses the environment name so that shell status bars (e.g., zsh prompt showing current directory) display the correct project name.

### Plugin System

All crates except `enwiro` (core) and `enwiro-logging` (shared library) are **standalone binaries** discovered at runtime by naming convention:

- `enwiro-cookbook-*` — provides recipes (blueprints for environments)
- `enwiro-adapter-*` — integrates with a window manager
- `enwiro-bridge-*` — integrates with a launcher/UI

Plugins communicate via subprocess: core calls `<plugin> list-recipes` and `<plugin> cook <recipe>` and reads stdout. This means manual testing requires `cargo install --path <crate>` to make the binary visible to the core.

### Crate Relationships

```
enwiro (core CLI)
├── calls → enwiro-cookbook-git     (discovers git repos/branches as recipes)
├── calls → enwiro-cookbook-chezmoi (chezmoi source dir as recipe)
├── calls → enwiro-cookbook-github  (discovers GitHub repos via GraphQL API)
├── calls → enwiro-adapter-i3wm   (i3 workspace integration)
├── calls → enwiro-bridge-rofi    (rofi launcher UI)
└── uses  → enwiro-logging        (shared library, workspace dependency)
```

### Key Traits (in core)

- `CookbookTrait`: `list_recipes()` → `Vec<Recipe>` (name + optional description), `cook(&str)` → `String` (path)
- `EnwiroAdapterTrait`: `get_active_environment_name()`, `activate(name)`
- `Notifier`: desktop notifications for success/error events

`CommandContext<W>` holds config, adapter, cookbooks, notifier, a generic writer (real stdout or `Cursor<Vec<u8>>` in tests), and `cache_dir: Option<PathBuf>` (set to a tempdir in tests to isolate from the real daemon).

### Per-Environment Metadata

Each environment directory contains a `meta.json` file (`EnvStats` struct in `enwiro/src/usage_stats.rs`) storing frecency stats (activation count, last activated timestamp), cookbook origin, and description. Functions: `load_env_meta`, `record_activation_per_env`, `record_cook_metadata_per_env`. Legacy centralized `usage-stats.json` is checked as fallback for unmigrated environments.

### Background Recipe Cache Daemon

`list-all` pre-caches recipe listings via a self-managing background daemon to avoid blocking the UI on slow cookbook plugins (e.g., GitHub API calls). All logic lives in `enwiro/src/daemon.rs`.

- **Auto-start**: `list-all` calls `ensure_daemon_running()` which spawns `enwiro daemon` (hidden subcommand) if no daemon is running
- **Auto-exit**: daemon exits after 1 hour of inactivity (no `list-all` calls touching the heartbeat file)
- **Refresh**: every 5 minutes, the daemon re-discovers plugins and writes `recipes.cache` atomically
- **Staleness**: cache older than 5min 30s is treated as missing — `list-all` falls back to synchronous collection
- **Runtime files** in `$XDG_RUNTIME_DIR/enwiro/` (fallback `$XDG_CACHE_HOME/enwiro/run/`): `daemon.pid`, `recipes.cache`, `heartbeat`
- **PID liveness**: checked via `libc::kill(pid, 0)`; stale PID files are handled gracefully
- **Signals**: SIGTERM, SIGINT, SIGHUP cause clean shutdown (PID file removed)

### Git Cookbook (most complex crate)

Single-file implementation in `enwiro-cookbook-git/src/main.rs`. Recipe discovery flow:

1. Glob-match repo paths from config
2. For each repo: discover existing worktrees → collect checked-out branches → enumerate local then remote branches
3. Existing worktrees become `ExistingRepo` recipes, branches become `Branch` recipes
4. Cooking a `Branch` recipe creates a git worktree on-demand in `$XDG_DATA_HOME/enwiro/worktrees/`
5. Enwiro-managed worktrees (prefixed `enwiro-`) are hidden from discovery to keep branch recipes stable

Recipe names use `@` separator: `repo-name@branch-name`. Slashes in branch names are flattened with hash suffix for filesystem paths.

### GitHub Cookbook

Implementation in `enwiro-cookbook-github/src/main.rs`. Discovers GitHub issues/PRs via GraphQL API and creates worktree-based environments. Recipe names use `repo#number` format (e.g., `SeaGOAT#1015`) — the owner prefix is stripped to stay consistent with the git cookbook's naming.

## Testing Patterns

- `rstest` fixtures for the core crate (see `enwiro/src/test_utils.rs` for mocks: `FakeCookbook`, `FailingCookbook`, `EnwiroAdapterMock`, `MockNotifier`)
- `tempfile::TempDir` for filesystem tests
- Git cookbook tests create real git repos with `git2` and verify worktree creation
- TDD workflow: write failing test first, then implement the fix

## Release System

Uses `release-plz` with workarounds:
- Publishing disabled in release-plz config due to gitoxide bug — handled by custom `cargo publish` loop in CI
- `release_commits` filter prevents infinite version bump loops from "chore: release" commits
- Config in `release-plz.toml`, workflow in `.github/workflows/release-plz.yaml`
