---
title: i3wm Adapter
description: User guide for the enwiro i3 window manager adapter.
---

The i3wm adapter connects enwiro to the [i3 window manager](https://i3wm.org/).

## Installation

```sh
cargo install enwiro-adapter-i3wm
```

## Configuration

If this is your only installed adapter, enwiro auto-selects it. Otherwise, set
it in your enwiro configuration (`~/.config/enwiro/enwiro.toml`):

```toml
adapter = "i3wm"
```

### Browser for gear URLs

When an environment has gear with web URLs, the adapter opens them
automatically on activation. By default it uses Chromium in app mode
(`chromium --app=<url>`), which opens each URL as a chromeless window.

To use a different browser, create
`~/.config/enwiro/adapter-i3wm.toml`:

```toml
web_open_command = ["firefox", "--new-window", "{url}"]
```

The `{url}` placeholder is replaced with the actual URL. The first element is
the browser command; remaining elements are arguments.

### Rebalance rate limit

Automatic [workspace rebalancing](#workspace-rebalancing) runs at most once
per debounce window (default: 5 seconds). To change it, set
`rebalance_debounce_secs` in the same `adapter-i3wm.toml`:

```toml
rebalance_debounce_secs = 120
```

## Features

### Workspace activation

`enw activate <name>` creates a new i3 workspace for the environment or
switches to it if one already exists. On first activation, gear URLs and GUI
applications are opened automatically. Re-activating an existing workspace
only switches focus (no duplicate windows).

### Workspace rebalancing

The adapter automatically rebalances workspace numbers based on each
environment's slot score. This keeps frequently used environments on
lower-numbered (more accessible) workspaces. The rebalancing algorithm
minimizes the number of workspace moves, so your keyboard shortcuts stay stable
as much as possible. Rebalancing runs on workspace switch events, rate-limited
by a configurable debounce interval (`rebalance_debounce_secs`, see
[Configuration](#rebalance-rate-limit)).

### Running commands

`enw run <command>` opens a new terminal window (via `i3-sensible-terminal`)
with the command running inside it. If the command exits with a non-zero status,
the terminal stays open and shows the exit code so you can read any error output.

### GUI application auto-open

Environments with `linux-gui` gear entries (e.g., Obsidian, Zotero) are spawned
automatically on first activation. The adapter checks that each binary exists
on `PATH` before spawning, so partially installed setups work without errors.
