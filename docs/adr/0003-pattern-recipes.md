# ADR-0003: Pattern recipes for cooking unlisted names

## Status

Accepted

## Context

Cooking was gated on the daemon's recipe cache (`find_recipe_in_cache`,
enwiro/src/context.rs): a name not listed there could not be cooked. Some
cookable things cannot be enumerated - a git branch that doesn't exist yet
(#246), a GitHub issue not assigned to the user, a PR outside the search
window.

## Decision drivers

- Routing stays data-driven: no speculative, side-effectful `cook` calls to
  discover which cookbook owns a name.
- A typo'd repo part must still fail fast; only the free part of a name is
  open-ended.
- Old bridges/CLIs must degrade gracefully; an honest cookbook bug must not
  hijack routing for every name.

## Considered options

- ✓ **Regex claims in the cache** (`my-project@(?P<branch>.+)`): precise,
  cross-cookbook claims stay disjoint.
  ✗ *Prefix wildcards* - can't express suffix structure, overlaps degrade to
  priority-only. ✗ *Capability flag + try-cook-in-order* - probing with real
  `cook` calls has side effects.
- ✓ **Untagged wire enum, `pattern` instead of `name`**: every existing
  consumer already skips unparseable lines, so zero changes; `Concrete` is
  tried first so a stray `pattern` field can't flip a named recipe.
  ✗ *`pattern: true` flag on `Recipe`* - old consumers would display
  regexes as names.
- ✓ **`{group}` description templates via `leon`**: placeholder-only,
  exposes keys for validation against `capture_names()`.
  ✗ *`Captures::expand`* - silently substitutes empty for unknown groups.
  ✗ *`strfmt`/`tinytemplate`* - unneeded template-language surface.
- ✓ **No fallback between cookbooks on cook failure.**
  ✗ *Try the next match* - a transient failure would change which cookbook
  handles a name (GitHub down → git cookbook creates a literal
  `owner/repo#123` branch), and the description shown would be a lie.

## Decision

Cookbooks emit pattern items on the `listen` stream. The daemon validates
each (the RAW pattern must compile - standalone-valid patterns have balanced
parens, so the `^(?:...)$` anchor wrapper cannot be escaped; template keys
must be capture groups), anchors it, and appends it after all concrete cache
entries. The CLI resolves exact matches first, then patterns in cache
(priority) order; a pattern-routed cook surfaces the rendered description as
a notification and env description. Templates are stored exactly as
validated; only display text is capped at 200 chars. The git cookbook claims
`repo@<branch>` (fork from remote default, local HEAD when remote-less);
github claims `repo#[0-9]{1,19}`.

## Consequences

- Unlisted names become cookable with no protocol probing and no UI changes.
- Typos in the free part create real resources (branches, envs) - mitigated
  by the notification, not prevented.
- An old daemon drops the entire recipes update of an upgraded cookbook;
  acceptable in a monorepo released together (`just install-dev` restarts
  the daemon).
- Risk: claims wider than `cook` honors - nonexistent GitHub numbers mint
  junk envs; stale concrete entries silently re-create deleted branches;
  checked-out branches get a false "Create new branch" notification. Known,
  deferred (#246 review notes).

## Implementation notes

`enwiro-sdk/src/recipe_pattern.rs` (validate/anchor/match/escape),
`cookbook.rs`/`client.rs` (wire + cache enums), daemon `cached_pattern_entry`
(validation gate), CLI `find_recipe_in_cache` (two-pass resolution). Author
contract: docs/creating-a-cookbook.md § Pattern recipes.

## References

Issue #246.
