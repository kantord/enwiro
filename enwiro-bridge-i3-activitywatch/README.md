# enwiro-bridge-i3-activitywatch (deprecated)

**This crate is deprecated.** It has been renamed to
[`enwiro-bridge-activitywatch`](https://crates.io/crates/enwiro-bridge-activitywatch).

The bridge no longer talks to i3 directly: it asks the enwiro daemon which
environment is active, so it works with any adapter (i3wm, tmux, ...). Since
nothing i3-specific remains, the `i3` in this crate's name became misleading.

Migrate with:

```sh
cargo uninstall enwiro-bridge-i3-activitywatch
cargo install enwiro-bridge-activitywatch
```

The enwiro daemon starts and supervises the bridge automatically; no further
setup is needed. See the [enwiro documentation](https://enwi.ro/bridges/) for
details.

This package's binary is a stub that only prints a deprecation notice.
