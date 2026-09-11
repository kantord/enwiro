# ADR-0006: Resolving real descriptions for pattern-routed cooks

## Status

Accepted

## Context

ADR-0003 gave pattern-routed cooks a static template `description` (`"Work
on PR or issue #{number} in {repo}"`, `"Create new branch '{branch}' in
{repo}"`), rendered with no network lookup and frozen into `meta.json` at
cook time (`enwiro/src/context.rs`'s `save_cook_metadata` ->
`enwiro-daemon/src/meta.rs`'s `record_cook_metadata_per_env`). ADR-0003
flagged this as a known, deferred risk.

In practice this affects 91 of 286 live environments (32%, across 9 repos)
- tauler's workspace list renders the boilerplate template as the
workspace label instead of a real title (issue #803). A dotfiles-side
script partially worked around this for the github shape only, which is
itself the wrong place for enwiro business logic to live.

## Decision drivers

- `cook()`'s worktree-exists early return means anything resolved only
  inside `cook_pr`/`cook_issue` never runs again after the first cook - a
  fix must not depend on that path re-running.
- The rendered template description also powers an existing typo-safety-net
  toast (`resolve_and_cook`'s `notify_info`) shown before a pattern cook
  creates a new branch/env - a fix must not remove that signal.
- Two cookbooks (`git`, `chezmoi`) implement no title lookup at all; the
  fix must degrade to today's behavior for them with no new code.

## Considered options

- ✗ **Widen `CookbookTrait::cook`'s return type** to carry an optional
  description (`CookResult { path, description }`). Rejected: touches 2
  trait impls and 4 cookbook CLI protocols, and is unreachable on re-cook
  because of the worktree-exists early return - the exact regression this
  ADR exists to avoid.
- ✗ **Async self-heal**: daemon's periodic recipe-cache rebuild emits a
  `DescriptionChanged` event that updates `meta.json` in the background.
  Rejected: real correctness bugs found in review (unconditional
  overwrite with no unchanged-guard, unbudgeted new `gh` calls, no
  timeout), and it reopens the same "no last-user-set-wins guard" risk
  class as a daemon sweep in general - more new surface than a first pass
  needs.
- ✗ **Null the template at the source** (`item_pattern_recipes()` /
  `branch_pattern_recipes()`). Rejected: the same field feeds the
  `notify_info` typo-safety-net toast; nulling it there silently disables
  that UX.
- ✓ **`CookbookTrait::describe(&self, recipe: &str) -> Result<Option<String>>`**,
  an optional best-effort method mirroring the existing `gear()` /
  `external_paths()` shape exactly (same `best_effort_json_via_rpc` /
  `best_effort_json_via_subprocess` dispatch, same fail-open-to-`None`,
  same shared 10s `BEST_EFFORT_SUBCOMMAND_TIMEOUT`). Called only for
  pattern-routed cooks, from `resolve_and_cook`, after the static template
  has already been used for the toast. `cook()`'s signature and the
  static templates stay untouched. A cookbook that doesn't implement
  `describe` (git, chezmoi, obsidian today) gets the trait default
  `Ok(None)` for free - zero new code for them.

## Decision

Add `describe()` to `CookbookTrait` as above. `enwiro-cookbook-github`
implements it with a single `gh api repos/{repo}/issues/{number}` call
(covers both issues and PRs - a PR is distinguished by the presence of the
`pull_request` key in the response, avoiding a second sequential `gh`
call and its timeout risk). Like `gear()`, the cookbook binary's stdout is
parsed directly as the trait's `Option<T>` payload (`T = String` here) -
it prints a bare JSON string (e.g. `"[PR] Fix auth bug"`), not a wrapper
object. `resolve_and_cook` persists
`describe()`'s result instead of the static template for pattern-routed
cooks, and also uses it (falling back to the static template on `None`)
for the `notify_info` toast, so the toast shows the same real title that
gets persisted.

The result is resolved once, at first successful cook, and frozen
thereafter - not kept live. A later title change upstream (PR renamed,
etc.) will not be reflected. This is a deliberate, accepted trade-off:
correct-until-write beats the complexity of a live-refresh mechanism for a
one-line UI label. A `description_source: auto | manual` field is added to
`meta.json` now (default `auto`, set by every `describe()`-driven write)
so that a future manual override command has a field to check before
being silently overwritten - no such command exists yet.

## Consequences

### Positive

- Real titles for pattern-routed cooks, no new I/O beyond one already
  network-bound cookbook subprocess call, no signature changes to `cook()`.
- Structurally immune to the re-cook regression that killed the first
  proposal: `describe()` never touches a worktree, so it's unaffected by
  whether `cook`'s early return has already fired.
- A committed `enw meta refresh` backfill (not a throwaway script) clears
  the existing 91-env backlog and is reusable if a similar gap appears
  for a future cookbook.

### Negative / Trade-offs

- Descriptions can still go stale after a later title edit upstream -
  same shape of bug as today, just far rarer. Explicitly accepted, not
  fixed (see Decision).
- `describe()` is a new required-to-consider method on every
  `CookbookTrait` impl (default no-op, so no cookbook is forced to
  implement it, but it is a slightly larger trait surface).

### Risks

- `record_cook_metadata_per_env`'s "`None` leaves the field untouched"
  semantics, which correctly protects a transient `describe()` failure at
  re-cook time, would silently no-op the backfill for git-cookbook envs
  (whose `describe()` is always `Ok(None)`) if the backfill reused that
  same write path. Mitigated: the backfill's placeholder-clear case
  writes through `load_env_meta`/`save_env_meta` directly instead.

## Implementation notes

- `enwiro-sdk/src/client.rs`: `describe()` on `CookbookTrait`, adapted from
  `gear()` in `RpcCookbookClient` and `CookbookClient`.
- `enwiro-cookbook-github/src/main.rs`: `Describe` CLI variant, single
  `gh api repos/{repo}/issues/{number}` call, `[PR]`/`[issue]` prefix by
  presence of `pull_request` key, same sanitization as `recipes_for_item`.
- `enwiro-daemon/src/meta.rs`: `EnvStats.description_source: auto | manual`
  (default `auto`).
- `enwiro/src/context.rs`: `resolve_and_cook` calls `describe()` for
  pattern-routed cooks; result feeds both the persisted description and
  the `notify_info` toast (falling back to the static template on `None`).
- `enw meta refresh [env_name] [--all] [--dry-run]`: new committed
  subcommand; matches placeholder-shaped descriptions, dispatches through
  the daemon's existing name-keyed cookbook registry (no per-cookbook
  logic in the command itself), no throttling (91 calls is well under
  GitHub's authenticated rate limit).

## Related decisions

Extends ADR-0003, which deferred this exact risk ("stale concrete
entries... false notifications").

## References

Issue #803.
