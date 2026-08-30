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
* [`enw prep`↴](#enw-prep)
* [`enw rm`↴](#enw-rm)
* [`enw run`↴](#enw-run)
* [`enw shell`↴](#enw-shell)
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
* `prep` — Cook (if needed) and print the env path; no adapter contact
* `rm` — Remove an environment
* `run` — Run a command via the active environment's adapter
* `shell` — Run your shell inside the current environment, waiting while it is being prepared. Intended as a terminal emulator's configured shell: with no environment it degrades to the plain shell.
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



## `enw wrap`

Run an application/command inside an environment

**Usage:** `enw wrap <COMMAND_NAME> [ENVIRONMENT_NAME] [-- [CHILD_ARGS]...]`

###### **Arguments:**

* `<COMMAND_NAME>`
* `<ENVIRONMENT_NAME>`
* `<CHILD_ARGS>`
