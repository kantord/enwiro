---
title: Development setup
description: Build and install enwiro from a local checkout.
---

## Prerequisites

- A [Rust toolchain](https://rustup.rs/) (`cargo`).
- The [`just`](https://github.com/casey/just) command runner.

## Build and install

Clone the repository, then from its root:

```sh
just install-dev
```

This builds the workspace in release mode and installs the enwiro binaries into
`~/.cargo/bin` (replacing any installed from crates.io), restarting the daemon if
it is running.
