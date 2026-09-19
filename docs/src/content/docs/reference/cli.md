---
title: CLI reference
description: Every enw subcommand, generated from the CLI's own help text.
---

This page is generated from the `enw` CLI definitions with `just docs-cli` - do not edit it by hand.

In addition to the subcommands below, `enw [-y] :<gear> [entry]` dispatches
to an environment's [gear](/launching-apps/) entries directly.

This document contains the help content for the `enw` command-line program.

**Command Overview:**

* [`enw`↴](#enw)
* [`enw activate`↴](#enw-activate)
* [`enw goal`↴](#enw-goal)
* [`enw goal show`↴](#enw-goal-show)
* [`enw goal set`↴](#enw-goal-set)
* [`enw goal clear`↴](#enw-goal-clear)
* [`enw info`↴](#enw-info)
* [`enw kanban`↴](#enw-kanban)
* [`enw ls`↴](#enw-ls)
* [`enw mark`↴](#enw-mark)
* [`enw meta`↴](#enw-meta)
* [`enw meta refresh`↴](#enw-meta-refresh)
* [`enw prep`↴](#enw-prep)
* [`enw rm`↴](#enw-rm)
* [`enw run`↴](#enw-run)
* [`enw shell`↴](#enw-shell)
* [`enw stale`↴](#enw-stale)
* [`enw stale ls`↴](#enw-stale-ls)
* [`enw stale prune`↴](#enw-stale-prune)
* [`enw wrap`↴](#enw-wrap)

## `enw`

**Usage:** `enw [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `activate` — Activate a workspace for a given environment, creating it if needed. Use NAME=RECIPE to create environment NAME from the recipe of RECIPE; if NAME already exists the recipe part is ignored.
* `goal` — Show, set, or clear the current environment's goal
* `info` — Show information about an environment
* `kanban` — interactive kanban board of environments grouped by status
* `ls` — list existing environments and/or available recipes
* `mark` — Set the status of the current environment
* `meta` — Refresh environment metadata resolved once at cook time
* `prep` — Cook (if needed) and print the env path; no adapter contact
* `rm` — Remove an environment
* `run` — Run a command via the active environment's adapter
* `shell` — Run your shell inside the current environment, waiting while it is being prepared. Intended as a terminal emulator's configured shell: with no environment it degrades to the plain shell.
* `stale` — List or remove environments that have not been used in a while
* `wrap` — Run an application/command inside an environment

###### **Options:**

* `--env <ENV>`



## `enw activate`

Activate a workspace for a given environment, creating it if needed. Use NAME=RECIPE to create environment NAME from the recipe of RECIPE; if NAME already exists the recipe part is ignored.

**Usage:** `enw activate [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--no-hooks` — Skip garnish `run_on: [Cook]` autorun hooks when cooking the env



## `enw goal`

Show, set, or clear the current environment's goal

**Usage:** `enw goal [COMMAND]`

###### **Subcommands:**

* `show` — Print the current goal (default when no subcommand is given)
* `set` — Set the current environment's goal to free text
* `clear` — Clear the current environment's goal



## `enw goal show`

Print the current goal (default when no subcommand is given)

**Usage:** `enw goal show [OPTIONS]`

###### **Options:**

* `--json` — Output as JSON



## `enw goal set`

Set the current environment's goal to free text

**Usage:** `enw goal set <TEXT>`

###### **Arguments:**

* `<TEXT>`



## `enw goal clear`

Clear the current environment's goal

**Usage:** `enw goal clear`



## `enw info`

Show information about an environment

**Usage:** `enw info [OPTIONS] [NAME]`

###### **Arguments:**

* `<NAME>` — Name of the environment to query. Defaults to the active environment

###### **Options:**

* `--json` — Output as JSON



## `enw kanban`

interactive kanban board of environments grouped by status

**Usage:** `enw kanban`



## `enw ls`

list existing environments and/or available recipes

**Usage:** `enw ls [OPTIONS]`

###### **Options:**

* `--all` — Show both environments and recipes (default)
* `--envs` — Show only existing environments (does not require the daemon)
* `--recipes` — Show only available recipes (requires the daemon cache)
* `--json` — Output in JSON lines format
* `--status <STATUS>` — Filter environments by status

  Possible values: `ready`, `active`, `waiting`, `done`, `evergreen`




## `enw mark`

Set the status of the current environment

**Usage:** `enw mark <STATUS>`

###### **Arguments:**

* `<STATUS>`

  Possible values: `ready`, `active`, `waiting`, `done`, `evergreen`




## `enw meta`

Refresh environment metadata resolved once at cook time

**Usage:** `enw meta <COMMAND>`

###### **Subcommands:**

* `refresh` — Re-resolve descriptions still stuck on their cook-time pattern-recipe template (ADR-0006), e.g. "Work on PR or issue #42 in myrepo"



## `enw meta refresh`

Re-resolve descriptions still stuck on their cook-time pattern-recipe template (ADR-0006), e.g. "Work on PR or issue #42 in myrepo"

**Usage:** `enw meta refresh [OPTIONS] [ENV_NAME]`

###### **Arguments:**

* `<ENV_NAME>` — Environment to refresh (defaults to the current environment)

###### **Options:**

* `--all` — Refresh every environment instead of just one
* `--dry-run` — Report what would change without writing anything



## `enw prep`

Cook (if needed) and print the env path; no adapter contact

**Usage:** `enw prep [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--no-hooks` — Skip garnish `run_on: [Cook]` autorun hooks when cooking the env



## `enw rm`

Remove an environment

**Usage:** `enw rm [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `-y`, `--yes` — Skip the confirmation prompt



## `enw run`

Run a command via the active environment's adapter

**Usage:** `enw run <COMMAND_NAME> [ENVIRONMENT_NAME] [-- [CHILD_ARGS]...]`

###### **Arguments:**

* `<COMMAND_NAME>`
* `<ENVIRONMENT_NAME>`
* `<CHILD_ARGS>`



## `enw shell`

Run your shell inside the current environment, waiting while it is being prepared. Intended as a terminal emulator's configured shell: with no environment it degrades to the plain shell.

**Usage:** `enw shell [OPTIONS] [SHELL_ARGS]...`

###### **Arguments:**

* `<SHELL_ARGS>` — Arguments forwarded verbatim to the shell (e.g. `-c <command>`)

###### **Options:**

* `--timeout <TIMEOUT>` — Seconds to wait for an environment that is still being prepared before falling back to a plain shell. 0 waits forever

  Default value: `30`



## `enw stale`

List or remove environments that have not been used in a while

**Usage:** `enw stale <COMMAND>`

###### **Subcommands:**

* `ls` — List environments that have not been used in a while
* `prune` — Remove done, stale environments and prune their cookbook resources



## `enw stale ls`

List environments that have not been used in a while.

Environments with an `evergreen` status are never listed, regardless of --days - that status exists specifically to mark environments meant to persist indefinitely.

**Usage:** `enw stale ls [OPTIONS]`

###### **Options:**

* `--days <DAYS>` — Consider an environment stale after this many days without activity

  Default value: `30`
* `--json` — Output in JSON lines format



## `enw stale prune`

Remove environments that are both stale and whose status is `done` (merged/closed). Other stale environments (active, waiting, ready, or unknown status) are left untouched. Each removed environment's owning cookbook is given a chance to clean up whatever it materialized for it (e.g. a git worktree).

**Usage:** `enw stale prune [OPTIONS]`

###### **Options:**

* `--days <DAYS>` — Consider an environment stale after this many days without activity

  Default value: `30`
* `-y`, `--yes` — Skip the confirmation prompt



## `enw wrap`

Run an application/command inside an environment

**Usage:** `enw wrap <COMMAND_NAME> [ENVIRONMENT_NAME] [-- [CHILD_ARGS]...]`

###### **Arguments:**

* `<COMMAND_NAME>`
* `<ENVIRONMENT_NAME>`
* `<CHILD_ARGS>`
