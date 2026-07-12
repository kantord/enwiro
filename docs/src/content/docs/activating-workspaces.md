---
title: Activating environments
description: Switch to any unit of work by name — enwiro brings your tools along and sets it up on demand if it doesn't exist yet.
---

Enwiro switches you between self-contained units of work - a feature branch, a
pull request, a vault, an experiment - each with its own folder and context. You
activate one by name and your tools (terminal, editor, browser, agents) follow
it together.

An [environment](/#environment) is such a unit of work that already exists. A
[recipe](/#recipe) is a blueprint enwiro can turn into an environment on demand.

You do not need to worry if an environment exists already - recipes and environments
are addressed the same way, and if you attempt to use a recipe, enwiro just
converts it into an environment on the fly.

## Activating

```sh
enw activate <NAME>
```

`activate` switches to the matching unit of work, creating it first if it
doesn't exist yet.

To run a single command inside an environment instead of switching to it, use
`enw wrap` (it sets one up on demand too):

```sh
enw wrap <COMMAND> [-- [COMMAND_ARGS]...]
```

## Listing what you can activate

```sh
enw ls
```

`enw ls` shows everything you can activate: existing environments and the
recipes you could still cook. (Both `enw ls` and `enw activate`
need the daemon running.)

## Names that aren't listed

Some names work without appearing in `enw ls`: `myrepo@some-new-branch`
creates that branch in a fresh worktree, and `myrepo#123` opens any issue or
PR by number, assigned to you or not. A notification tells you what is being
created - watch it, since a typo'd name creates a branch with exactly that
name.

## Different ways to work on the same thing

Several cookbooks can describe the *same* underlying unit of work or context differently, for example the same
git branch:

- the GitHub cookbook offers `repo#42` (a pull request), and
- the git cookbook offers `repo@pr-42` (the branch that pull request created).

Both lead to the *same* git worktree. While it hasn't been set up yet, you may
see more than one way to reach it and can pick whichever you like. Once it
exists as an environment, enwiro recognises the equivalent recipes and stops
listing them, so you simply activate the one thing, regardless of which
cookbook's name you (or enwiro) used to create it.
