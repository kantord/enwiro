---
title: Activating workspaces
description: How to switch to environments and recipes, and why a recipe disappears once you have cooked it.
---

An [environment](/#environment) is a working directory enwiro can switch your
desktop to. A [recipe](/#recipe) is a blueprint for an environment that does not
exist yet — when you activate a recipe, enwiro creates ("cooks") the environment
on the fly.

## Switching to an environment or recipe

```sh
enw activate <NAME>
```

`activate` switches your desktop to the matching workspace. If `<NAME>` is an
existing environment, enwiro switches to it. If it is a recipe, enwiro cooks the
environment first, then switches to it.

To run a single command inside an environment instead of switching to it, use
`enw wrap`:

```sh
enw wrap <COMMAND> [-- [COMMAND_ARGS]...]
```

Both commands create the environment from a matching recipe if it does not exist
yet.

## Listing what you can activate

```sh
enw ls
```

`enw ls` shows your existing environments together with the recipes you can
still cook. Use it to discover what is available to activate. (Both `enw ls` and
`enw activate` need the [daemon](/#daemon) running.)

## Why a recipe disappears after you cook it

Once you cook a recipe, the environment exists — so enwiro stops offering the
recipe in `enw ls`. There is nothing left to cook; you would just activate the
environment directly.

This also works **across cookbooks**, even when the names differ. The same git
branch can be reachable through more than one recipe:

- the GitHub cookbook offers `repo#42` (a pull request), and
- the git cookbook offers `repo@pr-42` (the branch that pull request created).

Both cook the *same* worktree. After you cook either one, enwiro recognises the
other as already cooked and hides it, so you are not offered a recipe for work
you have already set up.

While neither has been cooked, **both stay listed** — enwiro does not pick one
for you. You choose which recipe to cook; enwiro only removes the ones that are
already done.
