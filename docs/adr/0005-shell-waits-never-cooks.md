# ADR-0005: `enw shell` waits for cooking, never cooks

## Status

Accepted

## Context

`enw shell` (#783) lets a terminal emulator use enwiro as its configured
shell: it launches the user's shell inside the active environment, or a
plain shell when there is none.

The active environment's name comes from the adapter, and today the
adapter-resolved path never cooks: `get_or_cook_environment` only cooks
explicitly named environments (`enwiro/src/context.rs:481-496`); `enw wrap`
falls back to `$HOME` otherwise (`enwiro/src/commands/wrap.rs:36-47`). But in
the normal flow a cook is already in flight when the terminal opens:
`enw activate` switches the workspace first and then cooks inline in its own
process (`enwiro/src/commands/activate.rs:79`, `:86`). There is no
cook-in-progress state anywhere - a cook is either finished (the env dir
resolves) or not - and no daemon event stream to subscribe to
(`events.subscribe` is deferred in ADR-0002).

So a terminal opening in a freshly activated workspace has three options:
run unwrapped in `$HOME` (today's wrap behavior), start its own duplicate
cook, or wait for the activation's cook to finish.

## Decision drivers

- A terminal's configured shell must always produce a usable terminal and
  must never nag on every window.
- Cookbooks are documented as idempotent (re-runnable), not concurrent-safe;
  two simultaneous cooks of the same recipe (e.g. two git clones into one
  directory) are a real corruption risk.
- No cook-in-progress signal exists; the only cheap, reliable readiness
  signal is the environment directory resolving
  (`Environment::get_one`), which only succeeds once the final symlink of a
  cook has landed.

## Considered options

- ✓ **Chosen - shell waits, never cooks.** If the adapter-resolved name has
  a recipe in the daemon cache but no environment yet, poll for the
  environment to appear (spinner on stderr), bounded by a timeout; then fall
  back to a plain shell. One cook owner (activation), no race.
- ✗ **Rejected - shell cooks adapter-resolved names.** Covers workspaces
  created outside `enw activate`, but duplicates the activation's in-flight
  cook with no coordination.
- ✗ **Rejected - wait, then cook on timeout.** Re-introduces the race and
  doubles the worst-case wait.

## Decision

Activation owns cooking; `enw shell` only observes. It waits (default 30s,
`--timeout`, 0 = forever) for the environment to appear when a matching
recipe exists in the cache, and otherwise - no environment, no recipe, no
adapter, daemon unreachable, or timeout - silently degrades to the plain,
unwrapped shell. Only the timeout prints a one-line warning.

## Consequences

### Positive

- Exactly one process ever cooks a given environment in interactive flows.
- `enw shell` is safe as a terminal emulator's default shell: worst case is
  a plain shell after the timeout.

### Negative / Trade-offs

- Recipe-alias activations (`enw activate foo=x`) miss the cache lookup for
  `foo` and get a plain shell immediately while the cook runs - same as
  wrap today.
- Workspaces created outside `enw activate` never trigger a cook from the
  shell.
- Progress is a generic spinner: without a daemon event stream the
  activating process's cook steps cannot reach the shell process.

### Risks

- A failed activation cook leaves the shell waiting out its full timeout.
  Mitigation: the timeout itself, plus activation's existing error
  notifications.

## Implementation notes

`enwiro/src/commands/shell.rs`: `resolve_environment` (the decision),
`wait_for_environment` (the poll + spinner), `choose_shell`
(`$SHELL` unless it points back at enwiro, then the passwd login shell,
then `/bin/sh`). Launch parity with `wrap` via
`resolve_launch_via_daemon` (`enwiro/src/commands/wrap.rs`).

## Related decisions

- ADR-0002: the deferred `events.subscribe` would allow real cook progress
  in the spinner; `env_cooked` is a candidate event kind.
- #522 (daemon-side cooking) would give cooks a single owner by
  construction and could supersede the polling here.

## References

- Issue #783 (`enw shell`).
