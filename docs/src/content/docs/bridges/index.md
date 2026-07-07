---
title: Bridges
description: How enwiro integrates with external applications through bridge plugins.
sidebar:
  label: Overview
---

Bridges provide close integration between enwiro and other applications. A
bridge translates between enwiro and an external tool's interface, acting as
glue that connects the two.

## Available Bridges

- **enwiro-bridge-rofi** - Browse and activate environments from
  [rofi](https://github.com/davatorium/rofi). Invoked by rofi itself in
  script mode; nothing runs in the background.

- **enwiro-bridge-i3-activitywatch** - Reports the focused i3 workspace's
  enwiro environment to [ActivityWatch](https://activitywatch.net/) as
  heartbeats, so your time tracking knows which environment you were working
  in. Runs as a long-running process managed by the daemon.

## Installing a Bridge

Install the bridge binary (e.g., `cargo install enwiro-bridge-rofi`) and make
sure it is on your `PATH`. Like all enwiro plugins, bridges are discovered by
their binary name (`enwiro-bridge-*`).

Ephemeral bridges like the rofi bridge need to be hooked into the application
they integrate with (e.g., a rofi mode configuration). Long-running bridges
that declare the `listen` capability are started automatically by
`enwiro-daemon` - no systemd unit or manual process management needed.

## How the Daemon Manages Bridges

At startup, the daemon probes every discovered bridge with the `metadata`
subcommand. A bridge that declares the `listen` capability gets its `listen`
subcommand spawned as a supervised child process (restarted if it dies). Its
stdout is forwarded to the daemon's log. Bridges that don't declare the
capability - or don't answer the probe at all - are left alone.

Note that this means placing a binary named `enwiro-bridge-*` on your `PATH`
causes the daemon to execute it (with the `metadata` argument) at startup.

## Creating Your Own

See [Creating a Bridge](/bridges/creating-a-bridge/) for the protocol
specification.
