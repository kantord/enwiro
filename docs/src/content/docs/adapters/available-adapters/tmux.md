---
title: tmux Adapter
description: User guide for the enwiro tmux adapter.
---

The tmux adapter connects enwiro to [tmux](https://github.com/tmux/tmux),
mapping environments to tmux sessions.

## Installation

```sh
cargo install enwiro-adapter-tmux
```

## Configuration

If this is your only installed adapter, enwiro auto-selects it. Otherwise, set
it in your enwiro configuration (`~/.config/enwiro/enwiro.toml`):

```toml
adapter = "tmux"
```

## Features

### Workspace activation

`enw activate <name>` creates a tmux session for the environment or switches to
it if one already exists. If you are already inside tmux, the adapter uses
`switch-client` to change sessions. If you are outside tmux, it uses
`attach-session` to attach to the session.

New sessions start with your `$SHELL` wrapped via `enw wrap`, so the shell
inherits the environment's working directory and `ENWIRO_ENV`.

### Running commands

`enw run <command>` opens a new tmux window in the environment's session with
the command running inside it. If the session does not exist yet, it is created
first.

If you run `enw run` from outside tmux, a note is printed to stderr with the
`tmux attach` command you can use to see the new window.

### Activity tracking

The adapter polls the active tmux session at a configurable interval (default:
5 seconds) and emits workspace-switch events when the session changes. This is
used by the enwiro daemon for activity tracking.
