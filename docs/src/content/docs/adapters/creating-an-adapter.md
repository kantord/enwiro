---
title: Creating an Adapter
description: How to build a custom enwiro adapter for your window manager or any application with a workspace-like abstraction.
---

An adapter is a standalone program that connects enwiro to your window manager
or any other application that has a workspace-like abstraction. It translates
between enwiro's environment model and whatever workspace or session concept
your platform provides.

You can write an adapter in any language. Enwiro communicates with adapters by
running them as subprocesses and exchanging data over stdin/stdout.

## How Enwiro Finds Your Adapter

Enwiro scans every directory in `$PATH` for executables whose name starts with
`enwiro-adapter-`. The part after that prefix becomes the adapter name.

**Examples:**

- `enwiro-adapter-sway` - adapter name `sway`
- `enwiro-adapter-hyprland` - adapter name `hyprland`
- `enwiro-adapter-my-wm` - adapter name `my-wm`

The binary must have executable permissions. Non-executable files are silently
ignored. Enwiro also checks the directory containing its own executable, so
co-locating your adapter binary there works too.

If only one adapter is installed, enwiro auto-selects it. If you have multiple
adapters installed, set `adapter` in your configuration file
(`~/.config/enwiro/enwiro.toml`) to choose which one to use:

```toml
adapter = "sway"
```

## Subcommands

Your adapter binary must handle three required subcommands passed as the
first argument (`get-active-workspace-id`, `activate`, `run`), plus the
optional `metadata` and `listen` pair for daemon-driven switch events.

### `get-active-workspace-id`

```
enwiro-adapter-yourname get-active-workspace-id
```

Print the name of the currently active environment to stdout. No stdin is
provided. Enwiro trims surrounding whitespace from the output.

This is used by `enw wrap` to determine which environment the user is currently
in. If the user is not in any recognized environment, print an empty string.

Exit with code 0 on success. On failure, exit non-zero and write an error
message to stderr.

### `activate <name>`

```
echo '<payload>' | enwiro-adapter-yourname activate my-env
```

Switch to (or create) a workspace for the named environment. The environment
name is passed as a positional argument after `activate`.

Enwiro pipes a JSON payload to the adapter's stdin with the following shape:

```json
{
  "version": 1,
  "managed_envs": [
    {"name": "project-a", "slot_score": 0.8},
    {"name": "project-b", "slot_score": 0.3}
  ],
  "gear": {}
}
```

**Fields:**

- **`version`** - Protocol version (currently `1`). Your adapter can match on
  this to handle future protocol changes gracefully.
- **`managed_envs`** - List of all environments enwiro currently manages, each
  with a `slot_score` (a float used for workspace ordering/placement). For
  example, the [i3wm adapter](/adapters/available-adapters/i3wm/) uses
  this for workspace rebalancing. Adapters that don't need it can ignore it.
- **`gear`** - Opaque JSON object containing per-environment gear data. Adapters
  that support auto-opening URLs or GUI applications can walk this structure
  (see [Gear](#gear) below). Adapters that don't need this feature can ignore
  the field entirely.

All fields are optional and have sensible defaults.

Exit with code 0 on success. On failure, exit non-zero and write an error
message to stderr.

### `run`

```
echo '<payload>' | enwiro-adapter-yourname run
```

Spawn a command in a new window, pane, or terminal within the adapter's
platform. Enwiro pipes a JSON payload to stdin:

```json
{
  "version": 1,
  "env_name": "my-env",
  "env_path": "/home/user/.enwiro_envs/my-env",
  "command": "nvim",
  "args": ["src/main.rs"]
}
```

**Fields:**

- **`version`** - Protocol version (currently `1`).
- **`env_name`** - The environment name.
- **`env_path`** - Absolute filesystem path to the environment directory.
- **`command`** - The command to run.
- **`args`** - Arguments to pass to the command (may be empty).

**Requirements:**

Your adapter **must** ensure the spawned process has:

1. Its working directory set to `env_path`.
2. The environment variable `ENWIRO_ENV` set to `env_name`.

How the command is spawned is up to the adapter: a new terminal window (i3wm),
a new tmux window (tmux), or any other mechanism appropriate for the platform.

Exit with code 0 on success. On failure, exit non-zero and write an error
message to stderr.

### `metadata`

```
enwiro-adapter-yourname metadata
```

Print a JSON object to stdout declaring the adapter's optional capabilities -
the same plugin-metadata convention cookbooks and bridges follow:

```json
{"capabilities": [{"name": "listen"}]}
```

Each entry is an object with a `name` (so future capabilities can carry
parameters); names the host doesn't recognize are ignored. The subcommand is
optional in the sense that the daemon treats any failure (unknown command,
non-zero exit, no answer within a few seconds) as "no capabilities" - but
without it, the daemon will not spawn your [`listen`](#listen) subcommand
and switch events stay off.

Because this is a probe, the daemon **invokes your binary with `metadata`
at startup whether or not you implement it**. Your binary should therefore
exit non-zero promptly on any unrecognized subcommand, rather than falling
back to its normal behavior. Standard argument parsers (clap, argparse, a
shell `case` with a `*)` arm) already behave this way, so most adapters
need no extra work to satisfy this.

### `listen`

```
enwiro-adapter-yourname listen
```

**Declare the `listen` capability in your [`metadata`](#metadata) output, or
the daemon will not spawn this.** Start a long-running process that emits
workspace-switch events to stdout as JSON lines (one JSON object per line).
The daemon reads these events to track which environment is currently active.

Each event must have this shape:

```json
{"type": "workspace_switch", "env_name": "my-env", "timestamp": 1700000000}
```

**Fields:**

- **`type`** - Always the string `"workspace_switch"`.
- **`env_name`** - The environment name that was switched to.
- **`timestamp`** - Unix timestamp (seconds since epoch) of the switch.

The adapter should emit an event whenever the user switches workspaces or
sessions. How you detect switches depends on your platform: i3/sway provide
IPC event subscriptions, tmux requires polling.

The daemon spawns `listen` with no arguments. Rate limiting is the
adapter's own business: emit events promptly (within a few seconds of a
switch, so activity tracking stays accurate), and if any internal cadence
is worth tuning, read it from your adapter's own config file rather than
argv (see `rebalance_debounce_secs` in the i3wm adapter for an example).

The daemon terminates the listen process when it shuts down.

## Gear

The `gear` field in the activate payload contains structured data about
applications and URLs associated with each environment. Adapters that want to
auto-open applications on activation can walk this structure.

The gear JSON has this shape:

```json
{
  "<gear-name>": {
    "description": "Human-readable description",
    "web": {
      "<entry-name>": {
        "description": "Open the PR",
        "url": "https://example.com/pr/1"
      }
    },
    "linux-gui": {
      "<entry-name>": {
        "command": ["obsidian", "--vault", "/path/to/vault"]
      }
    }
  }
}
```

- **`web`** entries have a `url` field. The adapter can open these in a browser.
- **`linux-gui`** entries have a `command` field (an argv array). The adapter
  can spawn these as GUI processes.

Gear handling is entirely optional. If your adapter ignores the `gear` field,
activation still works normally. The gear structure is designed to be
forward-compatible: unknown fields and categories are silently ignored.

## Output Encoding

All stdout output must be valid UTF-8. If your binary produces invalid UTF-8,
enwiro treats it as an error.

## Error Handling

- **Exit code 0** means success - stdout is parsed as results.
- **Non-zero exit code** means failure - stdout is discarded and stderr is shown
  to the user as the error message.
- Adapter failures are reported to the user. Unlike cookbooks (where one failing
  cookbook doesn't break the recipe list), a failing adapter prevents the
  operation from completing.

## Example: A Minimal Adapter in Bash

Here is a minimal adapter skeleton. It does not implement real window management
but demonstrates the protocol:

```bash
#!/usr/bin/env bash
set -euo pipefail

case "${1:-}" in
    get-active-workspace-id)
        # Return the environment name from a hypothetical WM
        # In practice, query your window manager's API here
        echo "my-env"
        ;;
    activate)
        name="$2"
        # Read the JSON payload from stdin (optional: parse for gear/managed_envs)
        payload=$(cat)
        # Switch to or create a workspace named "$name"
        echo "Activating $name" >&2
        ;;
    run)
        # Read the JSON payload from stdin
        payload=$(cat)
        env_name=$(echo "$payload" | jq -r '.env_name')
        env_path=$(echo "$payload" | jq -r '.env_path')
        command=$(echo "$payload" | jq -r '.command')
        args=$(echo "$payload" | jq -r '.args[]' 2>/dev/null || true)

        # Spawn the command with ENWIRO_ENV set and cwd set to env_path
        cd "$env_path"
        ENWIRO_ENV="$env_name" exec "$command" $args
        ;;
    metadata)
        # Declare optional capabilities so the daemon spawns `listen`
        echo '{"capabilities":[{"name":"listen"}]}'
        ;;
    listen)
        # Emit workspace switch events as JSON lines
        # In practice, subscribe to your WM's event stream
        while true; do
            printf '{"type":"workspace_switch","env_name":"my-env","timestamp":%d}\n' \
                "$(date +%s)"
            sleep 5
        done
        ;;
    *)
        echo "Unknown subcommand: ${1:-}" >&2
        exit 1
        ;;
esac
```

Save this as `enwiro-adapter-example`, make it executable (`chmod +x`), and
place it anywhere on your `$PATH`. Set `adapter = "example"` in your enwiro
config to use it.

## How It All Fits Together

When a user runs `enw wrap <command>`:

1. Enwiro calls `get-active-workspace-id` on the configured adapter.
2. The returned name is matched to an environment in `~/.enwiro_envs/`.
3. The command is run with `ENWIRO_ENV` set and the working directory changed to
   the environment path.

When a user runs `enw activate <name>`:

1. Enwiro constructs an `ActivatePayload` with managed environments and gear.
2. The payload is piped to the adapter's `activate` subcommand via stdin.
3. The adapter creates or switches to the appropriate workspace.

When a user runs `enw run <command>`:

1. Enwiro constructs a `RunPayload` with the environment name, path, and command.
2. The payload is piped to the adapter's `run` subcommand via stdin.
3. The adapter spawns the command in a new window/pane with `ENWIRO_ENV` and the
   correct working directory.

When the enwiro daemon starts:

1. It probes the configured adapter with `metadata`.
2. If the adapter declares the `listen` capability, the daemon spawns its
   `listen` subcommand as a long-running child process; otherwise the adapter
   is left alone and switch events are disabled.
3. It reads workspace-switch events from the adapter's stdout. These events
   are used for activity tracking.

## Tips

- **`get-active-workspace-id` must be fast.** It runs synchronously every time
  the user invokes `enw wrap`.
- **`activate` can be slow.** Window manager IPC calls and workspace setup are
  expected here.
- **Best-effort gear handling.** If a gear URL or GUI command fails to open,
  log the error and continue. Don't let a missing browser or application
  prevent workspace activation.
- **Forward compatibility.** Use serde defaults or equivalent. If your adapter
  encounters unknown JSON fields in a payload, ignore them rather than failing.
  This keeps older adapters compatible with newer enwiro versions.
