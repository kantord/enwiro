# Contributing to Enwiro

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain)
- [just](https://github.com/casey/just) (command runner)

## Understanding the Plugin Architecture

Enwiro uses a plugin system where all crates except `enwiro` (core) and `enwiro-logging` (shared library) are **standalone binaries** discovered at runtime by naming convention (`enwiro-cookbook-*`, `enwiro-adapter-*`, `enwiro-bridge-*`).

Plugins communicate via subprocess — the core calls the plugin binary and reads stdout. This means that to manually test changes end-to-end, the modified binary must be installed in your `$PATH` (typically `~/.cargo/bin/`).

## Setting Up the Development Environment

First, install enwiro as a regular user would. Consult the [README](README.md) to understand which packages you need and how to configure them. For example:

```bash
cargo install enwiro
cargo install enwiro-cookbook-git
cargo install enwiro-adapter-i3wm    # only if you use i3
cargo install enwiro-bridge-rofi     # only if you use rofi
cargo install enwiro-cookbook-chezmoi # only if you use chezmoi
cargo install enwiro-cookbook-github  # only if you want GitHub integration
```

Then, switch to local development builds:

```bash
just install-dev
```

This reinstalls all your currently-installed enwiro binaries from the local repo. It only touches packages you already have installed, so your per-machine selection is preserved.

When you're done developing and want to go back to the stable crates.io versions:

```bash
just install-release
```

## Building and Testing

```bash
cargo build --workspace                    # Build all crates
cargo test --workspace                     # Run all tests
cargo test -p enwiro                       # Run tests for a specific crate
cargo test -p enwiro test_cook_environment # Run a single test by name
cargo clippy --workspace -- -D warnings    # Lint (CI treats warnings as errors)
cargo fmt --all --check                    # Check formatting
```

## Manual Testing

After making changes to a plugin crate, you need to reinstall it for the core to pick up the changes:

```bash
cargo install --path enwiro-cookbook-git    # Install a single crate
# or
just install-dev                                   # Reinstall all your enwiro packages from local repo
```

Then test with:

```bash
enwiro list-all          # List environments and recipes
enwiro list-environments # List only environments
enwiro activate <name>   # Activate an environment
```
