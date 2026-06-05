---
title: Activating workspaces
description: Activate any environment or recipe by name — enwiro sets it up on demand, so you never have to track what is already cooked.
---

The point of enwiro is that you activate something by name and it just works —
you should not have to know or care whether it has been "cooked" yet.

An [environment](/#environment) is a working directory enwiro can switch your
desktop to. A [recipe](/#recipe) is a blueprint for an environment that does not
exist yet. Both are activated the same way; if the environment isn't there yet,
enwiro creates ("cooks") it on the fly.

## Activating

```sh
enw activate <NAME>
```

`activate` switches your desktop to the matching workspace, cooking the
environment first if it doesn't exist yet. Whether `<NAME>` was an
already-cooked environment or a not-yet-cooked recipe is an implementation
detail you don't have to think about — either way you end up in the workspace.

To run a single command inside an environment instead of switching to it, use
`enw wrap` (it also cooks on demand):

```sh
enw wrap <COMMAND> [-- [COMMAND_ARGS]...]
```

## Listing what you can activate

```sh
enw ls
```

`enw ls` shows everything you can activate — existing environments and the
recipes you could still cook — as one list. (Both `enw ls` and `enw activate`
need the [daemon](/#daemon) running.)

## You see each thing once, not once per cookbook

Because you shouldn't have to care how something is set up, you also shouldn't
see the same thing twice under two different names. Several cookbooks can
describe the *same* underlying environment differently — for example the same
git branch:

- the GitHub cookbook offers `repo#42` (a pull request), and
- the git cookbook offers `repo@pr-42` (the branch that pull request created).

Both lead to the *same* git worktree. While it hasn't been set up yet, you may
see more than one way to reach it and can pick whichever you like. Once it
exists as an environment, enwiro recognises the equivalent recipes and stops
listing them — so you simply activate the one thing, regardless of which
cookbook's name you (or enwiro) used to create it.
