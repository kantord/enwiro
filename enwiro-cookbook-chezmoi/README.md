# enwiro-cookbook-chezmoi

A cookbook plugin for [enwiro](https://crates.io/crates/enwiro) that exposes your chezmoi source directory as an environment.

## Installation

```
cargo install enwiro-cookbook-chezmoi
```

## Usage

This cookbook provides a single recipe named `chezmoi`. When cooked, it creates an environment pointing to your chezmoi source directory (as returned by `chezmoi source-path`).

Switch to a workspace named "chezmoi" and enwiro will automatically set up the environment for you.

Requires [chezmoi](https://www.chezmoi.io/) to be installed.
