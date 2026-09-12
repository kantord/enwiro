# ADR-0006: Project-level isolation on microsandbox

## Status

Accepted

## Context

The `container-wrap` feature (issue #540) already runs an environment's
commands inside a prebuilt OCI image: if an image `enwiro/<env>` exists,
the daemon launches inside it, else on the host
(`enwiro-daemon/src/launch.rs`). The trigger is image presence; building
the image is out-of-band; the engine is podman with `--userns=keep-id`
and an optional krun microVM runtime; the project dir is bind-mounted at
its identical absolute path.

That implementation was built one discovered failure at a time, not from
a design, and the pattern is worth naming: every fix was a *git* command
breaking inside the container, each fixed narrowly, in sequence —
containers running as root left root-owned files on the host and git
refused to touch them (`dubious ownership`, fixed by running as the host
uid, #682); a worktree's `.git` points into its main repo's object
database, unmounted, so git commands failed outright in a worktree env
(#685, #690); a fresh container has no `~/.gitconfig`, so `git commit`
failed with `Author identity unknown` until identity was seeded
explicitly (#725, #727). Only one credential was ever solved end-to-end:
Claude's own OAuth token, via a bespoke host-side proxy and per-launch
capability token (`enwiro-daemon/src/proxy.rs`, #673–#683). Nothing else
touching the network was ever addressed — no SSH-agent forwarding, no
git-credential-helper passthrough, no registry-token passthrough, and no
egress restriction of any kind (`launch.rs` sets no `--network` flag at
all today). "Isolation makes basic things like `git commit`/`git push`
hard" is the accumulated cost of solving each of those narrowly instead
of once.

Four gaps block turning this into a real isolation substrate:

1. **No project concept.** Isolation is a per-codebase policy, but enwiro
   has no codebase identity to hang it on. The trigger is image presence
   per *environment* (per workspace/branch), so each branch of the same
   codebase needs its own image and there is no way to say "this
   codebase is isolated" once, for all its branches.
2. **No credential story beyond one CLI.** Claude's token is protected;
   git push, private registries, and `gh` are not. This is the actual
   "hard to use" complaint, not a missing egress policy.
3. **No unified access-control model across runtimes.** Plain
   podman/runc needs `--user`/`--userns=keep-id` to avoid root-owned
   files; `krun` ignores those flags entirely and does its own uid
   mapping. The fix for "who owns the files" was backend-specific, never
   designed once.
4. **No pluggable backend, and no need for one.** The launch decision is
   hardcoded podman+krun in `resolve_launch`. The project has since
   settled on microsandbox as the sole intended backend (see Decision),
   which removes the motivation for a backend-agnostic abstraction layer
   rather than creating one.

microsandbox (github.com/superradcompany/microsandbox) is a CLI (`msb`)
driving libkrun microVMs. It isn't validated by any independent
third party here, but it is validated by our own prior use: a separate
personal project's coding-agent dispatch tooling already runs it in
production, shelling out to `msb run`/`copy`/`exec`/`rm` as subprocesses, using
`--secret ENV@HOST` to scope an API key to one domain with
block-and-terminate on violation, and `--from-snapshot <name>` to boot a
prebuilt base. That usage is fully-permissive/ephemeral by design and
proves nothing about egress policy, but it does prove the CLI-subprocess
integration shape and the secret-scoping primitive work in practice.
`msb run --help` additionally documents a full network-policy engine
(`--net-rule` domain/CIDR allow/deny, `--net-default-egress`,
`-p HOST:GUEST` port forwarding, TLS interception) and per-mount
ownership (`-v`/`--mount-dir ... uid=<N>,gid=<N>`) that presents
host-owned files as a chosen guest owner without changing which user the
process runs as — a structural fix for the root-owned-files problem
instead of the current whole-container `--user` workaround.

## Decision drivers

- **Isolation is a project-level policy**, not a cookbook or recipe
  concern. Cookbooks are not what the user configures; recipes are
  ephemeral and dynamic (pattern recipes like `repo@branch`), so neither
  can carry per-codebase policy. The user wants to say "this codebase
  runs isolated", once.
- **Thin runner, generic images.** Enwiro does not build images and does
  not run repo-defined build steps (honors ADR-0001's "no autorun via the
  config system" rule). An image/snapshot is a generic, reusable base —
  not something built per project — so this holds equally for a coding
  project and a non-coding one (an Obsidian vault, a Google Drive
  folder): neither needs a bespoke build pipeline to turn isolation on.
- **Worktrees must be covered.** Git worktrees are a first-class enwiro
  env type, but they live outside the repo's directory tree, so the
  filesystem-ancestor config walker (`enwiro-sdk/src/config/mod.rs`) can
  never see the project's `.enwiro.toml` from inside a worktree. Policy
  must reach the env the cookbook creates.
- **Access control, not egress lockdown, is the real problem.** The
  feature's actual track record is git identity/ownership/worktree
  failures and one narrowly-solved credential — not a network leak.
  "Super easy to use" means basic git and dependency operations work
  inside isolation by default; a hardened egress policy is a different,
  later problem.
- **One backend, no seam.** Committing to microsandbox as the sole
  backend removes the reason to build or maintain a backend-agnostic
  abstraction. Re-introduce one only if a second backend is actually
  being built.

## Considered options

### Where isolation policy lives

- ✓ **Chosen — Project-level, in `.enwiro.toml`.** A named codebase
  identity ("project") is the home of per-codebase policy, carried by the
  project-level `.enwiro.toml` that ADR-0001's trusted-core walker already
  resolves. Extends an existing mechanism rather than inventing a config
  system.
- ✗ **Rejected — Recipe/cookbook-level `isolate` field.** Recipes are
  ephemeral/dynamic; pattern recipes have no entry to attach policy to,
  and cooking (produce a path) is orthogonal to wrapping (how to launch).
- ✗ **Rejected — Per-environment image presence (status quo).** Opaque
  (silent host fallback), per-branch, no way to express "never" or
  "always".

### How policy reaches a worktree env

- ✓ **Chosen — Cookbook is responsible for making the project config
  reachable in the env it creates**, via a symlink
  `<worktree>/.enwiro.toml -> <main-repo>/.enwiro.toml` at cook time.
  Trusted core stays filesystem-walk-only (no git knowledge); the git
  cookbook is the only component that knows the main repo path. Mirror of
  the existing `external_paths` mechanism (already used to bind-mount a
  worktree's main repo into the container today, #685).
- ✗ **Rejected — Trusted core follows git identity.** Would push git
  knowledge into trusted core; the walker would no longer be purely
  filesystem-based.

### Backend

- ✓ **Chosen — microsandbox, as the sole backend.** Replaces podman+krun
  outright; ripped out and replaced in place rather than run alongside it
  — one isolation implementation, one ownership model, no transition
  period carrying two.
- ✗ **Rejected — keep podman+krun (status quo) or add a proxy-allowlist
  adapter on top of it.** Preserves the two-runtime ownership-model split
  (#682's `--user` dance vs. krun's own uid mapping) this ADR exists to
  remove.
- ✗ **Rejected — hybrid (microsandbox + podman as a second tier).** Two
  code paths, no structural reason to keep the old one once microsandbox
  covers the need.

### Integration method

- ✓ **Chosen — subprocess to the `msb` CLI.** Same shape as today's
  podman argv-building code in `launch.rs` (spawn a process, build argv,
  parse output) — smallest migration diff, and the CLI surface is the
  one actually validated in production use elsewhere.
- ✗ **Rejected — embed the `microsandbox` Rust crate.** Tighter typed
  integration in principle, but unproven here: nobody has verified the
  crate exposes the same network/secret/mount feature surface as the
  CLI. Real risk of discovering gaps mid-implementation.

### The seam

- ✓ **Chosen — no seam.** Call microsandbox directly from the wrap layer.
  No trait, no backend-agnostic policy schema. A single-backend project
  gains nothing from the indirection today; add it back only when a
  second backend is actually being built.
- ✗ **Rejected — backend-agnostic isolation policy schema (trait +
  backend impls), derived from microsandbox's shape.** Speculative
  generality against a backend count of one.

### Isolation trigger and image resolution

- ✓ **Chosen — `isolate = true` in the project's `.enwiro.toml`; image
  name optional there, falling back to a user-level default in
  `~/.config/enwiro/enwiro.toml`.** `isolate = true` alone is enough for
  someone who has set up their own personal default snapshot; a project
  can still override with its own image name. No image resolves anywhere
  → fail loud, not a silent host fallback.
- ✗ **Rejected — enwiro-shipped global default image.** A single
  enwiro-owned default can't meaningfully fit both a Rust project and an
  Obsidian vault; baking in an opinionated default isn't this project's
  call to make.
- ✗ **Rejected — always require an explicit per-project image, no
  default at any level.** Leaves `isolate = true` doing nothing by
  itself even for someone who already has a personal default in mind.
- ✗ **Rejected — image presence alone as the trigger (status quo).**
  Opaque, per-environment not per-project, silent fallback.

### Ownership handling

- ✓ **Chosen — nothing.** Verified hands-on, before writing any argv code:
  microsandbox's mounts already present a host-owned directory as owned by
  the guest's own uid, in both directions (a file the guest creates as
  root comes back on the host owned by the real host user), independent of
  which uid the guest process runs as. `git commit` in a bind-mounted repo
  works with zero ownership flags. The `uid=`/`gid=` mount option this ADR
  originally planned to use turned out to solve a problem that no longer
  exists under this backend.
- ✗ **Rejected — per-mount `uid=<N>,gid=<N>`** (msb's `-v`/`--mount-dir`
  option), the ADR's original plan before this was verified. Unnecessary:
  see above.
- ✗ **Rejected — port the existing whole-container `--user <uid>:<gid>` +
  `HOME` rewrite.** Was the mechanism that diverged between podman and
  krun; also unnecessary now that ownership is solved structurally. `-u`
  is still set, but only for non-root *process* hardening, orthogonal to
  ownership.

### Network policy

- ✓ **Chosen — open by default; no `.enwiro.toml` network config surface
  in v1.** Matches microsandbox's own default (`--net-default-egress
  deny` with an implicit `allow@public` when no rules are set — i.e.
  effectively open). Isolation's v1 job is filesystem/process scoping and
  working credentials, not egress policy; the security value this ADR
  originally hung on network scoping is deferred, not delivered, in v1.
- ✗ **Rejected — secure by default (deny-all egress), curated default
  allowlist for common dev needs (git hosts, registries).** Keeps the
  original security posture but requires maintaining a curated allowlist
  as an ongoing cost, and the actual pain history was auth failures, not
  an egress leak — solving the wrong problem first.
- ✗ **Rejected — expose a simple `.enwiro.toml` domain-allowlist key
  now**, translated to `--net-rule`/`--net-default-egress deny`. Real
  capability, but adds config surface and a translation layer ahead of a
  proven need; microsandbox's full `--net-rule` engine remains available
  to add later without redesigning anything.
- **Deferred, not decided against — dev-server port forwarding**
  (`-p HOST:GUEST`), the other half of #296/#540's original ask. Real,
  separate feature (which ports, static vs. detected) with its own
  design; not bundled into this migration.

### Credential passthrough

- ✓ **Chosen — in scope for this ADR, preferring `--secret ENV@HOST`
  scoping.** SSH keys, git tokens, and registry credentials are as core
  to "a project's isolation policy" as mounts are. `--secret` keeps a
  credential out of the sandbox's inlined config and scopes it to the
  one host it's for, with `--on-secret-violation block-and-terminate` —
  directly answers the "proper access control" gap from the history.
  Raw mounts (e.g. an SSH agent socket, which has no `--secret`-shaped
  equivalent) remain the fallback where no host-scoped secret form fits.
- ✗ **Rejected — leave credential passthrough for a separate,
  later issue.** The ADR's whole subject is what a project's isolation
  policy carries; deferring credentials risks a second isolation-policy
  schema later instead of one now.
- ✗ **Rejected — plain mounts/env vars for everything, as today.**
  Repeats the exact "raw token exposed inside the sandbox" problem the
  Claude auth proxy was built specifically to avoid, for every other
  credential this ADR now covers.

### Claude authentication

- ✓ **Chosen — retire the bespoke host-side proxy and capability-token
  scheme (`enwiro-daemon/src/proxy.rs`) in favor of
  `--secret ANTHROPIC_TOKEN@api.anthropic.com`.** Same guarantee (the
  real token never enters the sandbox, and never inlines into config) via
  the same generic mechanism used for every other credential — one
  code path instead of two purpose-built ones.
- ✗ **Rejected — keep the custom proxy, use `--secret` only for the
  newly-in-scope credentials.** Avoids touching working code, but leaves
  two different credential-protection mechanisms coexisting for no
  structural reason.

## Decision

1. **Isolation is a project-level policy**, carried by `isolate = true`
   (and an optional image/snapshot name) in the project's `.enwiro.toml`,
   resolved by the existing trusted-core walker.
2. **The cookbook owns making the project config reachable** in the env
   it creates (worktree symlink), so policy follows the codebase across
   branches/worktrees — same pattern as the existing `external_paths`
   mechanism.
3. **microsandbox is the sole backend**, driven via subprocess to the
   `msb` CLI. Podman/krun are removed, not kept alongside it.
4. **No backend-agnostic seam.** The wrap layer calls microsandbox
   directly.
5. **No enwiro-shipped default image.** An unresolved image with
   `isolate = true` makes `launch.resolve` return an explicit error, which
   `enw wrap` treats exactly like any other resolve failure it already
   handles: a loud warning, then a bare host launch — not a silent,
   unannounced host fallback, but also not a new hard-refusal failure mode
   (the CLI has exactly one degrade path today; this reuses it rather than
   adding a second). A user may configure their own default snapshot name
   under `[isolation]` in `~/.config/enwiro/isolation.toml`.
6. **Ownership needs no special handling at all** (verified: microsandbox's
   mounts already map it correctly); `-u <host-uid>:<host-gid>` is set only
   for non-root process hardening, unrelated to ownership.
7. **Egress is open by default**; no network policy config surface ships
   in v1.
8. **Credential passthrough is a first-class part of this policy**,
   preferring `--secret ENV@HOST` scoping (SSH/git/registry credentials),
   falling back to mounts only where no secret-shaped equivalent exists.
9. **Claude's auth proxy is retired** in favor of the same `--secret`
   mechanism.
10. **Dev-server port forwarding is deferred** to a follow-up.

## Consequences

### Positive

- One codebase, one isolation policy, all branches/worktrees.
- Basic git operations (identity, ownership, worktree object access) and
  credentialed operations (push, private registries) work by default —
  directly addresses the feature's actual multi-year pain history,
  rather than the egress leak it was originally scoped around.
- One isolation implementation and one ownership model instead of two
  (podman's `--user` dance vs. krun's own mapping).
- One credential-protection mechanism (`--secret`) instead of a bespoke
  proxy plus ad hoc mounts.
- Stays a thin runner: no image building, no autorun of project files,
  works the same for coding and non-coding projects.

### Negative / Trade-offs

- A new first-class concept ("project") with discovery/lifecycle.
- Depending on a pre-1.0 third-party runtime (single lead maintainer,
  breaking 0.x releases, non-standard schema) — the whole isolation
  feature now rides on it with no fallback path.
- Egress is open by default: the original driver ("the one place
  containers genuinely beat worktrees — network scoping") is not
  delivered in v1. Isolation's practical benefit in v1 is filesystem and
  process scoping plus credential handling, not a network boundary.
- Removing podman/krun support is a breaking change for anyone with an
  existing `enwiro/<env>` OCI image relying on it.

### Risks

- **Silent host fallback.** Mitigated by design: `isolate = true` with no
  resolvable image makes `launch.resolve` return an explicit error, which
  `enw wrap` surfaces loudly (stderr + desktop notification) before it
  falls back to the host — the same treatment as a down daemon, so the
  fallback is never *unannounced*, even though it also isn't a hard
  refusal (see Decision, point 5).
- **Worktree policy misses.** If the cookbook symlink isn't created, a
  worktree silently runs unisolated. Needs a guard/check.
- **Retiring `proxy.rs` on faith.** `--secret ... --on-secret-violation
  block-and-terminate` needs to be verified end-to-end (does violation
  really terminate the sandbox, does the secret really never appear in
  `msb inspect`/logs) before deleting the existing, working proxy code —
  don't remove the proven mechanism until the replacement is confirmed
  equivalent.
- **No egress boundary.** With v1 open by default, isolation should not
  be marketed or relied on as a network security boundary; that remains
  future work, not a regression from a promise this ADR made and broke.

## Implementation notes

Implemented. What actually landed, including a few corrections found only by
building and testing against the real `msb` CLI:

- Rewrote the isolation branch of `enwiro-daemon/src/launch.rs`
  (`resolve_launch`) to build `msb run` invocations (foreground, no `-d`)
  instead of podman ones; removed all podman/krun-specific code
  (`--user`/`--userns=keep-id` handling, `CONTAINER_ENGINE`, image-tag
  sanitization, `is_krun_runtime`, `half_host_memory_mib`) and the
  `container_runtime` config setting end to end (`ConfigurationValues`,
  `DaemonConfig`, the RPC layer, `main.rs`).
- **Fail-loud is the existing degrade path, not a new one.** `enw wrap`
  already has exactly one failure mode for any `launch.resolve` problem: a
  loud stderr warning + desktop notification, then a bare unwrapped host
  launch (never a hard refusal). `isolate = true` with no resolvable image
  reuses that path (`resolve_launch` now returns `Result<_, String>`) rather
  than introducing a first-ever "refuse to launch" behavior, which would have
  been inconsistent with that invariant.
- **Ownership needed no new mechanism at all**, contrary to the original
  plan to use msb's per-mount `uid=`/`gid=` option: verified hands-on that
  microsandbox's bind mounts already present host-owned files as owned by
  the guest's own uid in both directions, with zero flags -- `git commit` in
  a mounted repo works out of the box. `-u <host-uid>:<host-gid>` is set
  anyway, purely for non-root process hardening (unrelated to the
  now-solved ownership question).
- **`msb`-on-`PATH` is checked explicitly** (`which`, kept as a dependency)
  as part of resolving isolation, mirroring the old podman-engine lookup --
  otherwise a missing `msb` binary would surface as a raw exec failure
  instead of the same degrade path as a missing image.
- Deleted `enwiro-daemon/src/proxy.rs` and its capability-token/shim
  machinery (and its now-unused deps: `bytes`, `http-body-util`, `hyper`,
  `hyper-util`, `reqwest`). Claude's token moves to
  `--secret <ENV>@api.anthropic.com` + `--on-secret-violation
  block-and-terminate`; the real value lives only in the `msb` process's own
  environment (never in argv, visible via `ps`/`msb inspect`). One
  correction found by testing: `--secret ENV@HOST` delivers the value inside
  the guest as `MSB_<ENV>`, not `<ENV>` -- the launch prelude renames it to
  `CLAUDE_CODE_OAUTH_TOKEN`, the name `claude` actually reads.
- Reused `enwiro_sdk::config::build_cookbook_config` (the existing
  project-config walker) directly for `isolate`/image resolution, scope
  `"isolation"` -- this is also where the user-level default naturally
  lands: **`~/.config/enwiro/isolation.toml`**, not bundled into the
  daemon's own `enwiro.toml`, since that's the file this loader already
  reads for any given scope with no new code.
- Reused the `external_paths` mechanism unchanged for worktree main-repo
  mounting, ported to msb's `-v SOURCE:DEST` mount syntax.
- **Known regression vs. podman**: `msb`'s mount flags have no colon-safe
  syntax (`--mount type=bind,source=,target=` had none to port); `msb`
  itself refuses a host path containing `:`, `,`, or `;` (verified hands-on)
  rather than mis-mounting it. Documented as a known limitation; not
  expected to matter in practice for enwiro project paths.
- New config surface: `[isolation]` `isolate`/`image` keys in `.enwiro.toml`
  and in `~/.config/enwiro/isolation.toml`.

## Related decisions

- ADR-0001 (project-level config) — provides the walker the policy rides
  on.
- ADR-0005 (`enw shell` waits, never cooks) — unaffected; isolation is
  wrap-layer.
- #540 (isolator/wrapper plugins) — this ADR is the design for it.
- #296 (visual containerization) — folded in; its network-scoping ask is
  deferred, not delivered, by this ADR's v1.
- #637 (microsandbox support) — resolved by this ADR; no longer a
  candidate, the chosen backend.
- #715 (wrapper recipes) — still flags "isolation profiles may be
  reimagined through this system"; still needs reconciling with the
  project-level framing here.
- #682, #685, #690, #725, #727 — the incremental podman-era fixes this
  ADR's access-control model replaces with a single design.

## References

- `enwiro-daemon/src/launch.rs` — the launch decision today.
- `enwiro-sdk/src/config/mod.rs` — the project-config walker.
- `docs/creating-a-cookbook.md` — the cookbook contract (`cook` returns a
  path; recipes are names, not config carriers).
- microsandbox — github.com/superradcompany/microsandbox (`msb` CLI:
  `run`, `exec`, `copy`, `rm`, `--secret`, `--net-rule`,
  `--mount-dir ... uid=/gid=`).
- Our own prior use of `msb`, in a separate personal project's
  coding-agent dispatch tooling — production usage of `msb` as a
  subprocess with `--secret ENV@HOST` scoping and `--from-snapshot`,
  validating the integration shape (though not its egress policy, which
  that usage runs fully open).
