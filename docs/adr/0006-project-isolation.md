# ADR-0006: Project-level isolation via an isolator seam

## Status

Draft — being grilled. Terms under discussion; backend and seam shape not
yet locked.

## Context

The `container-wrap` feature (issue #540) already runs an environment's
commands inside a prebuilt OCI image: if an image `enwiro/<env>` exists,
the daemon launches inside it, else on the host
(`enwiro-daemon/src/launch.rs`). The trigger is image presence; building
the image is out-of-band; the engine is podman with `--userns=keep-id`
and a krun microVM backend; the project dir is bind-mounted at its
identical absolute path.

The goal is to make this the **isolation substrate** of the project: a
thin runner that launches a project's apps on the host, in a container,
or in a microVM — nothing more. Enwiro does not own the agentic loop that
runs on top (that is the consuming project's concern).

Three gaps block that goal today:

1. **No project concept.** Isolation is a per-codebase policy, but enwiro
   has no codebase identity to hang it on. The trigger is image presence
   per *environment* (per workspace/branch), so each branch of the same
   codebase needs its own image and there is no way to say "this
   codebase is isolated" once, for all its branches.
2. **No egress policy.** `launch.rs` gives containers full outbound
   network. The one place containers genuinely beat worktrees — network
   scoping — is absent. #296 (GUI containerization, `--network none` by
   default) was folded into #540 and the network requirement was lost.
3. **No pluggable backend seam.** The launch decision is hardcoded
   podman+krun in `resolve_launch`. A future backend (remote VM hosts)
   would require reworking the wrap layer rather than slotting in.

## Decision drivers

- **Isolation is a project-level policy**, not a cookbook or recipe
  concern. Cookbooks are not what the user configures; recipes are
  ephemeral and dynamic (pattern recipes like `repo@branch`), so neither
  can carry per-codebase policy. The user wants to say "this codebase
  runs isolated", once.
- **Thin runner.** Enwiro does not build images and does not run
  repo-defined build steps. Stage 0's commit: enwiro runs a prebuilt
  image if one is available, else the host. This also honors ADR-0001's
  "no autorun via the config system" rule: enwiro never executes commands
  derived from project files.
- **Worktrees must be covered.** Git worktrees are a first-class enwiro
  env type, but they live outside the repo's directory tree, so the
  filesystem-ancestor config walker (`enwiro-sdk/src/config/mod.rs`) can
  never see the project's `.enwiro.toml` from inside a worktree. Policy
  must reach the env the cookbook creates.
- **Backend-agnostic for the future.** Local containers are not the end
  state; remote VM hosts are likely. The seam between the wrap layer and
  the backend must express enwiro-owned capabilities (mounts, ports, env,
  cwd, network egress) without leaking backend specifics.
- **Secure by default, with a dev-server carve-out.** The value of
  containers over worktrees is network scoping; egress should default to
  none, with an explicit, named carve-out so dev servers stay reachable
  on the host.

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
  the existing `external_paths` mechanism.
- ✗ **Rejected — Trusted core follows git identity.** Would push git
  knowledge into trusted core; the walker would no longer be purely
  filesystem-based.

### Network policy

- ✓ **Chosen — Default `none` (no outbound), with an explicit, named
  dev-server carve-out**: outbound + a stable host port mapping so the
  user's editor/browser reach the app. Honors #296's intent and #540's
  dev-server reality.
- ✗ **Rejected — Default full network (status quo).** The security value
  over worktrees disappears unless a project opts in.
- ✗ **Rejected — Per-command opt-in.** Requires knowing which apps need
  net ahead of time; wrong for a project policy.

### Backend

- **Candidate — microsandbox as the microVM backend.** A Rust crate
  (`cargo add microsandbox`) that runs OCI images as libkrun microVMs with
  host-side userspace (smoltcp) network-policy enforcement — so egress
  policy is real on macOS (Apple HVF), not just Linux. Per-VM ports,
  secrets, and a strict YAML config schema. Config is a per-sandbox
  runtime manifest (image ref, mounts, network, ports, secrets, scripts);
  it does **not** build images — a thin-runner fit. Caveats: pre-1.0,
  single lead maintainer, breaking 0.x releases, non-standard schema,
  and adopting it means it becomes the microVM backend (not a layer on
  podman+krun).
- **Candidate — keep podman+krun and build a thin proxy-allowlist
  adapter.** Preserves Stage 0 but rebuilds the policy/config layer the
  project wanted to avoid, and egress-as-proxy is a soft boundary with a
  documented bypass track record.
- **Candidate — hybrid.** microsandbox for microVMs, podman as a second
  container tier. Two code paths, most surface.

### The seam

- **Candidate — Enwiro owns a backend-agnostic isolation policy schema,
  with the backend config *derived* from it, starting from microsandbox's
  `Sandboxfile` shape** (so the config system is not invented from
  scratch). The seam is a trait/interface: enwiro policy → backend
  invocation. Microsandbox today, remote-VM host later, without the wrap
  layer changing.

## Decision

> Under construction — this is the draft being grilled.

What is settled so far:

1. **Isolation is a project-level policy** (a named codebase identity),
   carried by the project's `.enwiro.toml`, resolved by the existing
   trusted-core walker.
2. **The cookbook owns making the project config reachable** in the env
   it creates (worktree symlink), so policy follows the codebase across
   branches/worktrees.
3. **Egress defaults to none**, with a named dev-server carve-out
   (outbound + stable host port mapping).
4. **Enwiro owns a seam** so a future backend (remote VM host) slots in
   without reworking the wrap layer. The seam expresses enwiro-owned
   capabilities: mounts, ports, env, cwd, network egress.

Not yet decided:

- Whether microsandbox is adopted as the microVM backend (likely), and
  whether the existing podman+krun path remains a second container tier.
- The exact seam shape (enwiro-owned policy schema derived from
  microsandbox's, vs. something else) — prior-art research pending.
- How dev tools get host display/socket/port passthrough from inside the
  microVM (the "feels native" bar).

## Consequences

### Positive

- One codebase, one isolation policy, all branches/worktrees.
- The isolation boundary enwiro actually owns — network egress — becomes
  real.
- Backends become pluggable; remote VM hosts are a new backend, not a
  rewrite.
- Stays a thin runner: no image building, no autorun of project files.

### Negative / Trade-offs

- A new first-class concept ("project") with discovery/lifecycle.
- Depending on a pre-1.0 third-party runtime (if microsandbox is chosen)
  means inheriting its breaking-change churn and non-standard schema.
- The dev-server carve-out is a soft boundary unless paired with the
  hypervisor as the real isolation wall.

### Risks

- **Silent host fallback.** If the backend can't run (missing image,
  missing engine), a declared-isolated project must fail loud, not
  silently run untrusted code on the host.
- **Worktree policy misses.** If the cookbook symlink isn't created, a
  worktree silently runs unisolated. Needs a guard/check.
- **Egress allowlist is not a hard boundary.** Documented bypasses
  (CVE-2025-66479, null-byte injection) mean the network policy is an
  inner ring; the hypervisor is the wall.

## Implementation notes

- Extends `enwiro-daemon/src/launch.rs` (`resolve_launch`), which is the
  single launch-decision chokepoint.
- Reuses `enwiro-sdk/src/config/mod.rs` (the project-walker) for policy
  resolution.
- Reuses the `external_paths` mechanism as the pattern for cookbook
  declarations.
- New: the isolator seam (trait + backend impls); project-image tagging
  (one image per project, not per env).

## Related decisions

- ADR-0001 (project-level config) — provides the walker the policy rides
  on.
- ADR-0005 (`enw shell` waits, never cooks) — unaffected; isolation is
  wrap-layer.
- #540 (isolator/wrapper plugins) — this ADR is the design for it.
- #296 (visual containerization) — folded in; contributes the network
  requirement.
- #637 (microsandbox support) — candidate backend.
- #715 (wrapper recipes) — flagged "isolation profiles may be reimagined
  through this system"; needs reconciling with the project-level framing.

## References

- `enwiro-daemon/src/launch.rs` — the launch decision today.
- `enwiro-sdk/src/config/mod.rs` — the project-config walker.
- `docs/creating-a-cookbook.md` — the cookbook contract (`cook` returns a
  path; recipes are names, not config carriers).
- microsandbox — github.com/superradcompany/microsandbox (Apache-2.0,
  pre-1.0).
- Network-allowlist bypasses: CVE-2025-66479, SOCKS5 null-byte injection
  (fixed in sandbox-runtime 0.0.43 / claude 2.1.90).
