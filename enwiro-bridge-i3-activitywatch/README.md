# Enwiro ActivityWatch bridge (i3)

Report the focused i3 workspace's enwiro env to a local [ActivityWatch](https://activitywatch.net/) server, so time spent in each environment shows up in the usual aw-server dashboards and rollups.

## Status: temporary

This bridge depends directly on i3 because the daemon does not yet expose an
outbound event stream. Once [#432](https://github.com/kantord/enwiro/issues/432)
("expose env-activation events to external listeners") lands, this bridge
should be replaced by a WM-agnostic version that subscribes to the daemon
instead of talking to i3 directly.

Sway support comes for free if the i3-IPC compatibility layer is enabled;
it is in scope but only i3 is tested today.

## Installation

```
cargo install enwiro-bridge-i3-activitywatch
```

## Usage

Start a local aw-server (default port 5600), then run:

```
enwiro-bridge-i3-activitywatch
```

The bridge connects to i3, polls the focused workspace every 5 seconds, and
sends one ActivityWatch heartbeat per tick with a 15-second pulsetime (so
consecutive heartbeats inside the same env merge into a single duration
event).

Bucket id: `aw-watcher-enwiro_<hostname>` (event type `currentenv`).

## How it works

Workspaces are named `<num>: <env>` (e.g. `8: chezmoi`) by the i3 adapter, so
the workspace name is the authoritative signal for which env is focused.
Window-PID detection was tried first and turned out to be unreliable
(`_NET_WM_PID` is often unset and `/proc/<pid>/environ` is frozen at exec).

For each detected env, the bridge also reads metadata from
`${ENWIRO_ENVS_DIR:-~/.enwiro_envs}/<env>/meta.json` (`description`,
`cookbook`) and gear URLs from each `gear.d/*.json` (`gear.<name>.web.page.url`).
Gear URLs are flattened into `<source>-<gear>-url` keys (`github-issue-url`,
`obsidian-note-url`, ...) so aw-server's query layer can filter on them
without `json_extract` traversal. Metadata is cached for 10 seconds per env.

When no workspace is focused, the workspace name does not match the
`<num>: <env>` shape, **or the matched name has no corresponding directory
under `${ENWIRO_ENVS_DIR:-~/.enwiro_envs}/`**, the bridge emits no heartbeat
that tick. aw-server shows a gap in the timeline — same as any other
inactive period. There is no synthetic "no-env" event, and scratch
workspaces whose names happen to look enwiro-shaped are not tracked.

## Heartbeat shape

```json
{
  "timestamp": "2026-05-20T12:00:00Z",
  "duration": 0,
  "data": {
    "env": "chezmoi",
    "title": "chezmoi",
    "description": "...",
    "cookbook": "...",
    "github-issue-url": "https://github.com/kantord/enwiro/issues/327"
  }
}
```

`title` mirrors `env` (static for a given env) so aw-server's default
timeline view labels each row sensibly. `description` and `cookbook` come
from `meta.json`; gear URLs are added per-source.
