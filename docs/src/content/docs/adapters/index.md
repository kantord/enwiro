---
title: Adapters
description: How enwiro connects to your window manager or any application with a workspace-like abstraction.
sidebar:
  label: Overview
---

Adapters teach enwiro how your productivity environment deals with switching
between work contexts and displaying things like terminal sessions, web pages,
and other applications. Enwiro delegates platform-specific work to adapters,
which is how it can remain platform-agnostic.
[Not all uses of enwiro require an adapter.](#using-enwiro-without-an-adapter)

## Available Adapters

- **[i3wm](/enwiro/adapters/available-adapters/i3wm/)** - Makes
  [i3](https://i3wm.org/) workspaces working-directory-aware, similar to how
  tmux sessions work. Also auto-opens URLs and GUI apps on activation.

- **[tmux](/enwiro/adapters/available-adapters/tmux/)** - Maps environments to
  [tmux](https://github.com/tmux/tmux) sessions with automatic creation and
  attach/switch behavior.

## Installing an Adapter

Install the adapter binary (e.g., `cargo install enwiro-adapter-i3wm`).
**Enwiro auto-discovers installed adapters** - if only one is installed, it is
selected automatically and no configuration is needed.

### Multiple adapters

If you have multiple adapters installed, set which one to use in your enwiro
configuration (`~/.config/enwiro/enwiro.toml`):

```toml
adapter = "i3wm"
```

The value after `adapter =` is the adapter name - the part after the
`enwiro-adapter-` prefix of the binary.

## What an Adapter Does

An adapter is a standalone executable that handles four operations:

- **Identify the active environment** - tell enwiro which environment the user
  is currently in (used by `enw wrap`)
- **Activate an environment** - switch to or create a workspace/session for a
  given environment (used by `enw activate`)
- **Run a command** - spawn a command in a new window or pane within the
  environment (used by `enw run`)
- **Listen for switches** - emit events when the user changes
  workspaces/sessions (used by the daemon for activity tracking)

## Using Enwiro Without an Adapter

Some enwiro features work without an adapter. For example, `enw wrap` can run
commands in prepared environments using just the environment name - no window
manager integration needed. This is useful on headless servers, in CI, or
anywhere you want project-directory-aware command execution without graphical
workspace management.

## Creating Your Own

Adapters are fairly minimal - sometimes a simple shell script is enough. If
your application isn't supported yet, see
[Creating an Adapter](/enwiro/adapters/creating-an-adapter/) for the full
protocol specification.
