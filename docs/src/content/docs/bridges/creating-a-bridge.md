---
title: Creating a Bridge
description: The bridge plugin protocol - naming, the metadata subcommand, and the listen capability.
---

A bridge is a standalone executable that integrates enwiro with another
application. Unlike adapters and cookbooks, bridges have no fixed set of
operations - each bridge defines its own relationship with the tool it
integrates. The only protocol bridges share is discovery and the optional
`metadata` subcommand described here.

## Naming and Discovery

Name the binary `enwiro-bridge-<name>` and put it on `PATH`. Enwiro
discovers bridges the same way it discovers all plugins: by the binary name
prefix.

## The `metadata` Subcommand

Every enwiro plugin kind (cookbooks, adapters, bridges) shares one metadata
convention: a `metadata` subcommand that prints a JSON object with an
optional `capabilities` list declaring the plugin's optional abilities. For
bridges it looks like this - a JSON object printed to stdout, then exit:

```json
{ "capabilities": [{ "name": "listen" }] }
```

- `capabilities` - abilities the bridge declares to the daemon. Each entry
  is an object with a `name` so future capabilities can carry parameters.
  Consumers ignore capability names they don't recognize.

The subcommand is optional, and every failure mode is treated as "declares
nothing": if the probe cannot spawn, exits non-zero, produces output that is
not valid metadata JSON, or does not exit within a few seconds, the daemon
leaves the bridge alone. A bridge like `enwiro-bridge-rofi`, which is invoked
by rofi and needs no background process, simply prints `{}`.

Because this is a probe, the daemon **invokes your binary with `metadata`
at startup whether or not you implement it**. Your binary should therefore
exit non-zero promptly on any unrecognized subcommand, rather than falling
back to its normal behavior. A bridge with a non-argv-dispatched mode
(like rofi's script protocol) should answer `metadata` before its normal
dispatch, exactly as `enwiro-bridge-rofi` does.

## The `listen` Capability

Declaring `listen` tells the daemon this bridge wants to run as a
long-running background process. The daemon then:

1. Spawns `<bridge> listen` at daemon startup.
2. Restarts it if it exits (checked once per second).
3. Forwards each stdout line to the daemon's log. Bridge stdout is a log
   channel only - the daemon does not parse events from it.

Implement `listen` as a subcommand that runs until killed. Blocking forever
is fine; the daemon owns the lifecycle. Don't daemonize or fork - stay in
the foreground.

`enwiro-bridge-activitywatch` is the reference implementation: `metadata`
declares `listen`, and `listen` runs a watch loop that polls the daemon's
`env.current` RPC and heartbeats the active environment to ActivityWatch.
Daemon-spawned bridges inherit the RPC socket path via the environment, so
`enwiro_sdk::rpc::connect()` works out of the box.

## Argv Hygiene

The daemon executes every discovered bridge with the `metadata` argument at
startup. Make sure unexpected arguments never trigger side effects - handle
the subcommands you support and fail with a usage error otherwise. In
particular, a bridge driven by another program's protocol (like rofi script
mode) should answer `metadata` before its normal dispatch.
