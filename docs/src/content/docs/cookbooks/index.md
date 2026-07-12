---
title: Cookbooks
description: How enwiro discovers the things you can work on - repositories, branches, pull requests, vaults - and turns them into environments on demand.
sidebar:
  label: Overview
---

Cookbooks teach enwiro what you *could* be working on. Each cookbook watches
some source of work - local git repositories, GitHub issues, an Obsidian
vault - and offers what it finds as [recipes](/#recipe): names you can
activate that become [environments](/#environment) on demand. Turning a
recipe into an environment is called *cooking* it. Nearly everything you
can activate in enwiro comes from a cookbook.

## Available Cookbooks

- **[git](/cookbooks/available-cookbooks/git/)** - Turns your local git
  repositories, their branches, and their worktrees into environments. New
  branches get their own folder automatically.

- **github** - Surfaces open pull requests in your repositories, and issues
  assigned to you, as `repo#123` recipes - each becoming its own
  environment when you activate it. Requires an authenticated
  [`gh`](https://cli.github.com/) CLI.

- **chezmoi** - Exposes your [chezmoi](https://www.chezmoi.io/) dotfiles
  source directory as a permanent `chezmoi` environment (it is always
  there; nothing is created on demand).

- **obsidian** - Discovers your [Obsidian](https://obsidian.md/) vaults as
  `obsidian#vault-name` recipes and auto-opens Obsidian on activation.

## Installing a Cookbook

Install the cookbook binary with cargo, for example:

```sh
cargo install enwiro-cookbook-git
```

Enwiro discovers every installed `enwiro-cookbook-*` binary automatically.
The [daemon](/#daemon) keeps each cookbook's recipes refreshed in the
background, so recipe listing requires the daemon to be running.

## Configuring a Cookbook

Most cookbooks work out of the box and need no configuration. A few need to
be pointed at your data before they can find anything - the git cookbook,
for example, needs to know where your repositories live.

When a cookbook does take configuration, it reads its own file at
`~/.config/enwiro/cookbook-<name>.toml` (for example `cookbook-git.toml`).
The available keys are listed on each cookbook's page.

## Overlapping Recipes

Several cookbooks can describe the same underlying unit of work - for
example, the github cookbook's `repo#42` and the git cookbook's
`repo@pr-42` both describe working on pull request 42. Enwiro recognises
equivalent recipes and collapses them once an environment exists; see
[Different ways to work on the same thing](/activating-workspaces/#different-ways-to-work-on-the-same-thing).

## Creating Your Own

A cookbook is a standalone executable that lists recipe names and resolves
a chosen one to a directory - a shell script can be enough. See the
[cookbook authoring guide](https://github.com/kantord/enwiro/blob/main/docs/creating-a-cookbook.md)
for the full protocol specification.
