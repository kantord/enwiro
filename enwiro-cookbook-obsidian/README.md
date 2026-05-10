# enwiro-cookbook-obsidian

A cookbook plugin for [enwiro](https://crates.io/crates/enwiro) that exposes your Obsidian vaults as environments.

## Installation

```
cargo install enwiro-cookbook-obsidian
```

## Usage

This cookbook reads `~/.config/obsidian/obsidian.json` (the vault registry that Obsidian writes on Linux) and emits one recipe per vault, named `obsidian#<vault-slug>`. When two vaults share a basename, the slugs are suffixed `-1`, `-2`, etc., and their descriptions show the full vault path.

Switching to a workspace named after one of these recipes activates an enwiro environment pointing to the vault directory.

Currently Linux-only.
