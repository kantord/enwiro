---
title: Activating environments
description: Switch to any unit of work by name — enwiro brings your tools along and sets it up on demand if it doesn't exist yet.
---

enwiro switches you between self-contained units of work — a feature branch, a
pull request, a vault, an experiment — each with its own folder and context. You
activate one by name and your tools (terminal, editor, browser, agents) follow
it together.

An [environment](/#environment) is such a unit of work that already exists. A
[recipe](/#recipe) is a blueprint enwiro can turn into one on demand. You
activate either the same way — if it doesn't exist yet, enwiro creates it on the
spot, so you don't have to track which is which.

## Activating

```sh
enw activate <NAME>
```

`activate` switches to the matching unit of work, creating it first if it
doesn't exist yet. Whether `<NAME>` already existed or enwiro had to set it up is
invisible — either way you land in it.

To run a single command inside an environment instead of switching to it, use
`enw wrap` (it sets one up on demand too):

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
