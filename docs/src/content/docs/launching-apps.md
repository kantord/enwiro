---
title: Launching & isolating apps
description: How enwiro runs a command inside an environment — the wrap chokepoint, the daemon's launch decision, and the optional container isolation layer.
---

Everything enwiro launches in an environment goes through one command:
**`enw wrap`**. It is the single chokepoint where an environment's working
directory, its `ENWIRO_ENV` variable, and (optionally) container isolation are
applied before your program starts.

```sh
enw wrap <COMMAND> [ENVIRONMENT] [-- [COMMAND_ARGS]...]
```

`enw wrap bash my-project` runs `bash` inside the `my-project` environment. If
you omit the environment name, enwiro resolves the active one (and sets it up on
demand if it doesn't exist yet).

## How a launch is resolved

`enw wrap` does two things, in two different places:

1. **The CLI resolves (and, on demand, cooks) the environment.** This turns the
   environment name into a concrete project path. It stays in the `enw` process
   because cooking is interactive and local.
2. **The daemon decides _how_ to launch.** The CLI hands the resolved
   `(name, path, command, args)` to the daemon over its RPC socket
   (`launch.resolve`). The daemon is the single source of truth for the launch
   decision: it answers with the final program, arguments, and environment
   variables. The CLI then `exec`-replaces itself with that result, so your
   shell's terminal (the tty) stays attached to the launched process.

```mermaid
flowchart TD
    A["enw wrap COMMAND [ENV]"] --> B["CLI: resolve / cook the environment<br/>→ (name, path)"]
    B --> C{"daemon reachable?"}
    C -- "no" --> H["daemon down:<br/>stderr error + desktop notification,<br/>run COMMAND unwrapped<br/>(no env dir, no ENWIRO_ENV, no isolation)"]
    C -- "yes" --> D["daemon: launch.resolve<br/>(name, path, command, args, interactive)"]
    D --> E{"container-wrap feature on<br/>AND image enwiro/&lt;name&gt; exists?"}
    E -- "no" --> F["host launch:<br/>program = COMMAND<br/>env: ENWIRO_ENV=name"]
    E -- "yes" --> G["container launch:<br/>engine run --rm -it -v path:path -w path<br/>-e ENWIRO_ENV=name  enwiro/&lt;name&gt;  COMMAND"]
    F --> X["CLI sets cwd = path, applies env, exec()"]
    G --> X
    H --> X
    X --> Z["your program runs in the environment"]
```

In every case the launched program ends up with its working directory set to the
environment's path and `ENWIRO_ENV` set to the environment name, so tools and
shells can detect which environment they are in.

## The host path (default)

Out of the box, the daemon returns the command unchanged — it just runs on the
host, in the environment's directory, with `ENWIRO_ENV` set. This is the
behaviour you get without any isolation build flag.

## The container isolation path (optional)

enwiro can instead run your command inside a container, one image per
environment. This is **off by default** and gated two ways:

- The daemon must be **built with the `container-wrap` feature** (see below).
- A local OCI image named **`enwiro/<environment-name>`** must exist. Its mere
  presence is the trigger — building it is out of band (you bring your own
  image). If no such image exists, the launch falls back to the host path.

When both hold, the daemon returns a container invocation roughly equivalent to:

```sh
<engine> run --rm -it \
  -v <env-path>:<env-path> -w <env-path> \
  -e ENWIRO_ENV=<env-name> \
  enwiro/<env-name> <command> [args...]
```

- **Engine** is auto-detected: `podman` is preferred, then `docker`.
- The environment's project directory is **bind-mounted at the same path** it has
  on the host, and used as the working directory — so paths match and file
  watching/HMR work on a Linux host.
- `-it` is used when the caller's stdin is a terminal, `-i` otherwise.

> The image tag is `enwiro/<name>`, so the environment name must be a valid OCI
> tag. Names containing characters like `#` or `/` are not yet sanitised; use a
> simple-named environment when trying this out.

### Running with the isolation build flag

The container path lives behind the `container-wrap` Cargo feature on the
`enwiro-daemon` crate. The dev install recipe already builds it in:

```sh
just install-dev
```

This builds the whole workspace with
`cargo build --workspace --release --features enwiro-daemon/container-wrap`,
installs the binaries, and restarts the daemon — so the container path is
available. Because it is still image-gated, nothing changes until you create a
trigger image.

To build the daemon by hand instead:

```sh
cargo build --release -p enwiro-daemon --features container-wrap
```

### Try it end to end

```sh
# 1. Build + install with the feature (restarts the daemon)
just install-dev

# 2. Create a trigger image for a simple-named environment, e.g. "my-project"
echo 'FROM debian:stable-slim' | docker build -t enwiro/my-project -

# 3. Launch into it — you land in the container, at the bind-mounted project dir
enw wrap bash my-project

# An environment with no matching image still runs on the host:
enw wrap bash some-other-env
```

To turn the container path off again for an environment, remove its image
(`docker rmi enwiro/my-project`).

## Notes and limits

- **The daemon must be running.** It is the source of truth for how a command
  is launched. If it is down, `enw wrap` does not half-wrap: it prints an error
  to stderr, shows a desktop notification, and runs the command **unwrapped** —
  no environment directory, no `ENWIRO_ENV`, and no isolation.
- **`enw wrap` is the only launch path that consults the daemon today.** Other
  ways enwiro starts programs — `enw run` via an adapter, `enw :<gear>` cli
  entries, and the daemon's cook-autorun — still launch on the host and do not
  yet go through `launch.resolve`.
