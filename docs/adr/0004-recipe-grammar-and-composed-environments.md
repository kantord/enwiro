# ADR-0004: Recipe-name grammar and composed environments

## Status

Accepted

## Context

Composed environments (#375) let one environment hold several cooked
recipes side by side: `foo+bar` cooks both recipes and presents them as
sibling folders, for work that spans tasks (comparing two versions, a
rebase, a PR plus the issue it fixes). That requires an operator character,
and recipe names had no character rules at all: every cookbook-emitted name
flowed into the cache verbatim (`build_cache_content`,
enwiro-daemon/src/lib.rs), so a git branch `fix+v2` or a vault dir
`C++ notes` could legally produce names that any future grammar character
would collide with. Two more grammar features are already planned - `foo(x)`
wrapper recipes (#715) and parametric recipes (#723) - so the collision
problem compounds with every character the grammar will ever need.

## Decision drivers

- No cookbook may claim a character that later becomes an operator; the
  grammar must stay extensible without breaking user data a second time.
- Composition must be a single step (`enw activate foo+bar`), not a
  cook-then-link ritual.
- The parts of a composed env must remain plain envs at the worktree level:
  cooking a part standalone must reuse the same cooked project.
- One parser must own the syntax; ad-hoc `split('+')` calls sprinkled over
  core would drift the moment `()` nesting lands.

## Considered options

**Where names are constrained**

- ✓ **Allowlist, enforced centrally in trusted core.** Names may contain
  letters and digits (any script) plus `@` `#` `/` `.` `_` `-`. Concrete
  recipes are dropped with a warning at cache build
  (`cached_concrete_entry`); pattern claims cannot match an out-of-alphabet
  name (`recipe_pattern::match_name`), which also covers greedy claims like
  `repo@(?P<branch>.+)` swallowing `repo@fix+v2`.
- ✗ *Blocklist of today's grammar chars.* Every future operator would break
  user data again; a cookbook could still squat `!` or `~` in the meantime.
- ✗ *Per-cookbook discipline (docs only).* The github cookbook is already
  alphabet-clean, but git and obsidian provably were not; hope is not
  enforcement.

**Where composition lives**

- ✓ **Core grammar, parsed in the cook path** (`cook_environment`,
  enwiro/src/context.rs). Precedent: `enw activate NAME=RECIPE` aliasing is
  core syntax. The cookbook contract returns exactly one path per cook;
  composition inherently produces N paths plus a `main_folder` choice, which
  core alone assembles.
- ✗ *A `enwiro-cookbook-compose` plugin claiming a `+` pattern.* Needs the
  cook contract widened to multiple paths, and hands a grammar character to
  a plugin.
- ✗ *Plugin returning a self-made wrapper dir.* Keeps the one-path contract
  but hides the parts from core, blocking per-part features (sub-env
  commands, dedup).

**Parser**

- ✓ **chumsky + ariadne in enwiro-sdk (`recipe_expr`).** The v1 grammar
  (`expression := name ('+' name)*`) is trivial, but #715/#723 need a real
  parser; adding it now makes `recipe_expr::parse` the single source of
  truth from day one, with caret diagnostics distinguishing "reserved for
  grammar" from "not allowed".
- ✗ *`split('+')` until the grammar grows.* Cheap today, guaranteed
  re-plumbing later, and error messages would be string soup.

**Composed env shape**

- ✓ **Wrapper directory named like the env.** `<env>/<flat_name>/` is a
  real directory holding one symlink per part (flattened part name);
  meta.json gets `cookbook: "composed"` (a reserved plugin name), the full
  expression as `recipe`, and `main_folder` pointing at the wrapper.
  Entering the env shows all parts side by side - the primary use case.
  `resolve_project_symlink` accepts a directory as `main_folder`.
- ✗ *Part symlinks directly in the env dir, one part as main.* CWD lands in
  a single part; seeing both was the point. Env root as CWD would expose
  meta.json/gear.d noise.

## Decision

Recipe names are constrained to a central allowlist enforced by the daemon
and the pattern matcher; everything else is reserved recipe grammar. `+` is
the first operator: `a+b(+...)` (n-ary, order preserved, duplicate parts
rejected) cooks every part through normal cache resolution and assembles
one environment with a wrapper folder of part symlinks. The parser in
`enwiro_sdk::recipe_expr` is the only place the syntax exists. Blessed
name conventions: `#` container/item, `@` ref/variant, `/` hierarchy.

Per-part semantics: external paths are unioned (each part's cooked path
plus its cookbook's declared paths) under the `composed` contributor name,
because the wrapper's symlinks point outside the env. Gear and garnish
hooks are not collected for composed envs - their commands assume their own
part is the project dir; per-sub-env command routing is follow-up work, as
are worktree dedup and decomposing monorepos (#375 thread).

An alias is optional (`enw activate work=foo+bar`); without one the env is
named by the expression itself, which the filesystem accepts as-is.

## Consequences

### Positive

- Grammar can grow (#715, #723) without ever again colliding with data.
- Composition is one step, idempotent at the worktree level: cooking `foo`
  standalone and inside `foo+bar` share the same worktree, so "dedup" of
  identical parts is inherent rather than a layer.
- Parts stay individually activatable as their own envs.

### Negative / Trade-offs

- Recipes whose names already use banned characters (a `c++utils` repo dir,
  a `fix+v2` branch) disappear from listings, with only a daemon-log
  warning. Existing *envs* keep working; only re-cooking by recipe name is
  affected. The obsidian cookbook now slugifies into the alphabet instead.
- New branches whose names contain banned characters cannot be
  pattern-cooked (`repo@fix+v2` is parsed as composition and fails);
  existing branches remain reachable only if a concrete recipe lists them -
  it will not, since concrete names are also alphabet-checked. Rename the
  branch or add it via git directly.
- `(` `)` will need shell quoting when #715 lands; `+` composition itself
  is quote-free in every common shell.

### Risks

- A composed env's wrapper is not a git repo; tools that assume
  `environment.path` is a project may need the sub-folder. `main_folder`
  plus the wrapper's visible part symlinks keep this discoverable.
- Silent recipe loss from the allowlist could confuse users who do not read
  daemon logs. Mitigation: the drop is logged with cookbook and name; a
  future `enw doctor`-style surface could aggregate them.

## Implementation notes

- `enwiro-sdk/src/recipe_expr.rs` - alphabet, `parse` (chumsky), ariadne
  diagnostics, `COMPOSED_COOKBOOK_NAME`.
- `enwiro-sdk/src/recipe_pattern.rs::match_name` - alphabet gate before
  matching.
- `enwiro-daemon/src/lib.rs::cached_concrete_entry` - drop-with-warning.
- `enwiro-sdk/src/plugin.rs::PluginName` - reserves `composed`.
- `enwiro/src/context.rs` - `cook_environment` parses; `cook_plain_environment`
  / `cook_composed_environment` / `resolve_and_cook` split;
  `create_composed_wrapper`, `write_composed_external_paths`.
- `enwiro/src/environments.rs::resolve_project_symlink` - accepts a real
  directory as `main_folder`.

## Related decisions

- ADR-0002: `cookbook.invoke` remains the mechanism for cookbook-level
  *wrapping* (#715); composition deliberately does not use it.
- ADR-0003: pattern recipes are how parts of an expression resolve to
  unlisted names; the alphabet gate tightens `match_name`.

## References

- #375 (composed environments), #715 (wrapper-recipe syntax),
  #723 (parametric recipes), #246 (cook non-existent branches).
- docs/creating-a-cookbook.md, "Recipe names".
