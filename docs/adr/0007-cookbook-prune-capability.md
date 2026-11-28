# ADR-0007: Cookbooks declare `prune`, and a failed cleanup keeps the env

## Status

Accepted

## Context

Removing an environment asks its cookbook to tear down what `cook` created
(`prune_cookbook_resource` in `enwiro/src/commands/rm.rs`). The host ran
`prune` on every cookbook. A cookbook without that subcommand fails exactly
like one whose cleanup crashed, so the host deleted the environment after any
failure. The environment holds the only record of how to retry the cleanup:
with the daemon down, a git worktree stayed on disk with nothing pointing at
it.

## Decision drivers

- A failed cleanup must be retryable.
- Cookbooks with nothing to clean up must not get stuck or warn.

## Considered options

- ✓ **Chosen - a `prune` capability that means "implemented".** The host runs
  `prune` only if declared, so every failure is real
  ([#328](https://github.com/kantord/enwiro/issues/328)).
- ✗ **Rejected - run `prune` everywhere, and use the capability only to decide
  whether a failure keeps the env.** Keeps older cookbooks working, but needs
  do-nothing `prune` subcommands, and "declared" would not mean "implemented"
  as it does for `listen`.
- ✗ **Rejected - no capability; keep the env only when `prune` never ran or
  never finished.** Keeps environments of cookbooks with nothing to clean up
  while the daemon is down, and needs the host to classify RPC errors.

## Decision

- `prune` runs only on cookbooks that declare the `prune` capability.
- A declared `prune` that fails keeps the environment.
- `metadata` is required. If the probe fails, support is unknown, so the
  environment is kept.
- A config that fails to load also keeps the environment: with an empty config
  a cookbook would find nothing to prune and report success.

## Consequences

### Positive

- A failed cleanup can be retried by removing the environment again.
- Cookbooks without `prune` are removed without warnings.

### Negative / Trade-offs

- A cookbook that implements `prune` without declaring it is silently skipped,
  so cookbooks have to be updated before, or together with, `enw`.
- A cleanup that can never succeed leaves the environment until it is deleted
  by hand. The report names the directory.

### Risks

- A cookbook that reports success while leaving its resource behind (for
  example because it can no longer find the repository) is not covered.

## Related decisions

`gear` and the other optional subcommands are still run regardless of what the
cookbook declares; #328 stays open for them.
