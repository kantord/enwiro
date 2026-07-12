---
title: git
description: Turn your local git repositories, their branches, and their worktrees into enwiro environments.
---

The git cookbook turns your local git repositories into environments: the
repositories themselves and every branch you could work on. Each branch gets
its own separate folder, so switching to a branch never disturbs work you
have going on elsewhere - and activating a branch that does not even exist
yet simply creates it. When enwiro sets one of these up for the first time,
that step is called *cooking* the recipe.

## Installation

```sh
cargo install enwiro-cookbook-git
```

## Configuration

The cookbook finds your repositories through path patterns ("globs", where
`*` matches any name) listed in `~/.config/enwiro/cookbook-git.toml`:

```toml
repo_globs = ["/home/you/repos/*"]
```

- **`repo_globs`** (required) - a list of glob patterns; every match that is
  a git repository contributes recipes, other matches are ignored. Use
  absolute paths: `~` is **not** expanded.
- **`worktree_dir`** (optional) - base directory for the branch folders the
  cookbook creates. Defaults to `~/.local/share/enwiro/worktrees`.

Without `repo_globs` the cookbook finds nothing, so this is the one setting
every new enwiro install needs.

## Recipes

For a repository directory named `myrepo`, the cookbook offers:

- **`myrepo`** - the repository itself. Activating it takes you to the
  repository's own directory - the one created when you cloned it - with
  whatever branch it currently has checked out. Nothing is created or
  modified. Repositories are standing workspaces, so enwiro automatically
  marks them `evergreen`: they are never treated as finished work, unlike a
  branch you eventually close out.
- **`myrepo@<branch>`** - a branch of the repository, local or remote.
  Activating it gives the branch its own separate working folder (a
  [git worktree](https://git-scm.com/docs/git-worktree)), leaving the
  repository's own directory untouched. Remote branches appear under their
  short name (`origin/feature` is offered as `myrepo@feature`); a local
  branch with the same name takes priority.
- **`myrepo@<new-branch>`** - a branch that does not exist yet. Any name
  after the `@` works even though it is not in the recipe list: enwiro
  creates the branch and its folder for you. Watch the notification when
  activating - a typo'd branch name creates a branch with exactly that
  name.

If you already use `git worktree add` yourself, your hand-made worktrees
show up too, as **`myrepo@<worktree-name>`**.

### Why a branch may be missing from the list

You might expect every branch to appear as a `myrepo@<branch>` recipe, but
branches that are already checked out somewhere do not. Such a branch is
already reachable through the place that holds it - the repository itself
(`myrepo`), a worktree you made by hand, or an environment enwiro created
earlier - so offering it again as a branch recipe would only create a
duplicate. Your hand-made worktree still shows up, but under its worktree
name (`myrepo@<worktree-name>`), not as a second `myrepo@<branch>` entry.

This is also why a branch recipe disappears from the recipe list after you
activate it: it has become an environment, and you will find it among your
environments instead.

## Cooking Behavior

When a branch recipe is activated for the first time, the cookbook creates
its folder under `worktree_dir`; later activations reuse the same folder.

A branch that does not exist yet is created from the remote's default
branch as of your last fetch or pull (or from the current `HEAD` in
repositories without a remote). The cookbook never accesses the network, so
the starting point is only as fresh as your local clone.

If a branch is already checked out in another worktree, cooking fails with
an "already checked out" error - switch that worktree to another branch
first.

## Ordering

Recipes are ordered by recent local activity: repositories by when you last
staged, committed, or switched branches in them, branches by their latest
commit. The most recently touched work appears first in `enw ls` and in
launchers built on it.

## Technical Details

### Branch folder names

Looking inside `worktree_dir`, you will see folders like
`myrepo-3f8a91c2/my-branch-a41b09de`. The suffixes exist so folders never
collide: two repositories that share a directory name (in different
locations) get separate folders, and so do branch names that differ only
in `/` versus `-` (such as `feat/x` and `feat-x`). They are short hashes
derived from the repository's path and the branch's name, so the same
recipe always maps to the same folder. You never need to type them -
recipes are always addressed by their plain `myrepo@branch` name.
