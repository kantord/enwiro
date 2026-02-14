# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

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

Enwiro connects window manager workspaces to project environments. An environment is a symlink in `~/.enwiro_envs/` pointing to a working directory.

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
├── calls → enwiro-adapter-i3wm   (i3 workspace integration)
├── calls → enwiro-bridge-rofi    (rofi launcher UI)
└── uses  → enwiro-logging        (shared library, workspace dependency)
```

### Key Traits (in core)

- `CookbookTrait`: `list_recipes()` → `Vec<String>`, `cook(&str)` → `String` (path)
- `EnwiroAdapterTrait`: `get_active_environment_name()`, `activate(name)`
- `Notifier`: desktop notifications for success/error events

`CommandContext<W>` holds config, adapter, cookbooks, notifier, and a generic writer (real stdout or `Cursor<Vec<u8>>` in tests).

### Git Cookbook (most complex crate)

Single-file implementation in `enwiro-cookbook-git/src/main.rs`. Recipe discovery flow:

1. Glob-match repo paths from config
2. For each repo: discover existing worktrees → collect checked-out branches → enumerate local then remote branches
3. Existing worktrees become `ExistingRepo` recipes, branches become `Branch` recipes
4. Cooking a `Branch` recipe creates a git worktree on-demand in `$XDG_DATA_HOME/enwiro/worktrees/`
5. Enwiro-managed worktrees (prefixed `enwiro-`) are hidden from discovery to keep branch recipes stable

Recipe names use `@` separator: `repo-name@branch-name`. Slashes in branch names are flattened with hash suffix for filesystem paths.

## Testing Patterns

- `rstest` fixtures for the core crate (see `enwiro/src/test_utils.rs` for mocks: `FakeCookbook`, `EnwiroAdapterMock`, `MockNotifier`)
- `tempfile::TempDir` for filesystem tests
- Git cookbook tests create real git repos with `git2` and verify worktree creation
- TDD workflow: write failing test first, then implement the fix

## Release System

Uses `release-plz` with workarounds:
- Publishing disabled in release-plz config due to gitoxide bug — handled by custom `cargo publish` loop in CI
- `release_commits` filter prevents infinite version bump loops from "chore: release" commits
- Config in `release-plz.toml`, workflow in `.github/workflows/release-plz.yaml`
