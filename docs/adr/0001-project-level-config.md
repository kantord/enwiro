# ADR-0001: Project-level config via `.enwiro.toml`

## Status

Accepted

## Context

Every cookbook and adapter stores its config in a single user-global file at
`~/.config/enwiro/<scope>.toml`, loaded via `confy` v2.0.0. This works for
user-level preferences ("which adapter do I use") but not for per-project
tuning.

Pain points already on the roadmap:

- **#228 docker-compose cookbook** — per-project compose-file location and
  service names.
- **#227 scaffold cookbook** — per-project template list.
- **#226 monorepo cookbooks** — per-project workspace globs.
- **#206 hooks/templates** — not buildable without a per-project config layer.

Putting per-project keys in the user-global file leaks them across projects.
There's no way today to say "in this repo, set X" and have it travel with the
repo.

Also: every cookbook crate today hand-rolls a `pub struct ConfigurationValues`,
derives serde, calls `confy::load`, and defines its own defaults. At 8+ plugins
the boilerplate compounds. A shared loader removes it.

Backwards compatibility is **not** required — the project has no known users
yet, so the on-disk layout can change.

## Decision drivers

- **Per-project tuning** must live in a file inside the repo.
- **The trust boundary belongs in trusted code**, not in a Rust-only SDK
  helper that non-Rust cookbooks can bypass.
- **Plugin extensibility**: cookbooks can be any language, discovered by PATH
  prefix (`enwiro-cookbook-*`). The protocol can't assume Rust.
- **No autorun**: enwiro must not execute commands as a side-effect of reading
  configuration. Project-wide rule, inherited by every future config feature.
  Autorun features may exist, but only on paths where the script is derived
  from something the user explicitly installed or allowed (e.g. installing a
  cookbook).
- **Prefer mature, boring libraries** when no feature is load-bearing.
- **Don't preclude future work**: format and protocol must not block a future
  expression layer (CEL, #206) or cookbook inheritance (#301).

## Considered options

### Library choice

- ✓ **Chosen — `config = "0.14"` (config-rs).** 28M downloads, stable layered
  loading via `add_source`. Drops per-key provenance on merge, but the MVP
  doesn't need provenance (no `enw config explain` yet), so it doesn't
  matter. Boring tech wins.
- ✗ **Rejected — `figment = "0.10"`.** Per-key provenance, four merge
  strategies, `Jail` test sandbox. Last release Dec 2024; maintainer signaled
  renewed attention in Oct 2025 but hasn't shipped. A community fork
  (`figment2`) exists. The natural swap target if `enw config explain` ever
  lands.
- ✗ **Rejected — `confique`.** Derive-macro-first, generates annotated
  templates. Drops per-source provenance; adds a novel proc-macro to debug
  for negligible gain at four crates.

### On-disk format

- ✓ **Chosen — TOML.** Already used by `confy`; comments survive; section
  syntax (`[cookbook-git]`) maps naturally to the per-scope layout;
  `.editorconfig` and `Cargo.toml` precedent.
- ✗ **Rejected — JSON.** No comments, awkward for user-edited files.
- ✗ **Rejected — YAML.** Indentation sensitivity is a known footgun in
  human-edited configuration.

### Where the per-cookbook security boundary lives

- ✗ **Rejected — In the SDK.** A Rust helper reads project files, applies
  the allowlist, hands the cookbook its slice. Trivially bypassed: non-Rust
  cookbooks, old cookbooks, or anyone hand-rolling their own loader silently
  loses the safety.
- ✓ **Chosen — In trusted core (`enw` CLI + daemon).** Trusted code walks,
  parses, filters, and hands the cookbook a pre-resolved JSON blob.
  Cookbooks can't bypass it regardless of language or version.

### Cookbook protocol shape

- ✓ **Chosen — Stdin payload.** Mirrors the existing `AdapterPayload`
  pattern at `enwiro-sdk/src/adapter.rs:17-43`. Works in any language. No
  length limits.
- ✗ **Rejected — CLI flag (`--project-overrides <json>`).** Shell argv
  length limits; breaks symmetry with `AdapterPayload`.

### Whether to allowlist at all

- ✗ **Rejected — Default-merge everything.** Any key in `.enwiro.toml`
  overrides the user file. A hostile repo could set `workspaces_directory`
  and redirect every environment.
- ✓ **Chosen — Default-deny per-field allowlist.** Cookbook author opts each
  field in via `metadata.project_overridable`. Contains the blast radius of
  malicious or just-confused project files.

## Decision

1. **Add a project layer.** Trusted core (`enw` CLI and daemon) walks ancestors
   of the active CWD looking for `.enwiro.toml`. Sections within each file are
   keyed by cookbook/adapter scope (e.g. `[cookbook-git]`).

2. **Library: `config = "0.14"`.** Cookbook config structs use plain
   `#[derive(serde::Deserialize, Default)]` — no custom derive macro. Confy
   is removed from every cookbook crate. Swap to figment later if `enw
   config explain` ever lands; the loader is small (~80 lines) and a swap
   is one day of work.

3. **Format: TOML on disk, JSON on the wire.** The user-edited file is TOML
   for comments and Rust-ecosystem parity. The cookbook protocol payload is
   JSON because it crosses a language boundary.

4. **Trust boundary in trusted core.** Cookbooks never parse project files.
   Trusted code reads both the user-level file
   (`~/.config/enwiro/<scope>.toml`) and any discovered `.enwiro.toml`,
   filters the project layer through the cookbook's allowlist, merges, and
   injects the resolved config as JSON into the cookbook's stdin via a new
   `CookbookPayload` struct mirroring `AdapterPayload` at
   `enwiro-sdk/src/adapter.rs:17-43`.

5. **Default-deny per-field allowlist.** Each cookbook declares
   `project_overridable: [...]` in its `metadata` subcommand output. Trusted
   core silently drops any project-layer keys not on the list. Cookbooks
   without an allowlist see no project overrides at all. Initial allowlists:

   - `enwiro-daemon`: empty (`workspaces_directory`, `adapter` are
     user/system-level decisions).
   - `enwiro-cookbook-git`: `["repo_globs"]`.
   - `enwiro-cookbook-github`: empty initially.
   - `enwiro-adapter-i3wm`: empty initially.

6. **No-autorun policy (project-wide).** No enwiro config file — at any
   layer, present or future — may trigger command execution as a side-effect
   of being parsed or merged. Predicates (future CEL) evaluate data; hooks
   (future #206) run only via explicit user verbs like `enw activate`. In
   the MVP this is safe by construction (no exec-shaped fields exist);
   future work that introduces such fields must enforce the policy.

## Consequences

### Positive

- Per-project tuning has a natural home; #226/#227/#228 become buildable
  without coercing user-global config.
- Trust boundary is mechanical and language-agnostic: a hostile `.enwiro.toml`
  can only set fields the cookbook author opted in.
- Cookbooks lose the per-crate confy load + `ConfigurationValues`
  boilerplate; config plumbing becomes one SDK responsibility.
- Re-uses the `AdapterPayload` precedent at
  `enwiro-sdk/src/adapter.rs:17-43`, keeping the plugin protocol uniform.
- Format and protocol are inheritance-ready (future `metadata.extends`) and
  expression-ready (future CEL on typed string fields). Neither needs a
  format change later.

### Negative / Trade-offs

- Picking `config-rs` over `figment` defers per-key provenance. Library swap
  needed if `enw config explain` later lands (one day of work).
- `config-rs` default array semantics replace rather than concatenate. Some
  future use cases may want Cargo-style `adjoin` semantics, requiring a
  field-by-field merge override. Revisit if real users hit it.
- The no-autorun policy is documentation today, not code-enforced. Becomes
  enforceable only when there's something to enforce.

### Risks

- **Data-tampering** from a hostile project file: contained by the allowlist
  (cookbook author reviews each opt-in field). Worst case is "annoying
  surprise" (e.g. misdirected discovery glob), not RCE — the user already
  runs `cargo build` etc. on cloned repos, which dominates.
- **Allowlist drift**: cookbooks may add fields and forget to declare them.
  Acceptable — those fields just stay user-only until the author opts in.

## Implementation notes

- Cookbook protocol extension (new in `enwiro-sdk/src/cookbook.rs`):

  ```rust
  #[derive(Debug, Serialize, Deserialize, Default)]
  pub struct CookbookPayload {
      pub version: u32, // = 1
      #[serde(default)]
      pub config: serde_json::Value,
  }
  ```

  Future inheritance (see Related decisions below) adds an
  `inherited: Vec<ComposedConfig>` field where each element carries
  `{ type, name, config }`. Non-breaking because of `#[serde(default)]`.

- Metadata extension: cookbook `metadata` subcommand output gains an array
  field `project_overridable: ["field", ...]`. Missing field = empty
  allowlist.

- Trusted-core walker: in `enwiro-sdk::config`, call sites in `enw` CLI and
  the daemon. Walks `cwd.ancestors()` collecting `.enwiro.toml`s, applies
  outermost-first → innermost-wins ordering, filters per allowlist before
  merge.

- Per-cookbook adoption is ~5 lines: read stdin, deserialize the payload,
  deserialize the inner `config` into the cookbook's typed struct. Delete
  the existing `confy::load(...)` call; drop the `confy` dependency.

- Shell cookbooks read the payload with `cat` and extract values with `jq`.
  No SDK required — cookbook binaries can be any language.

- **Cookbook binaries are not for direct invocation.** They're spawned by
  `enw` / the daemon, which pipes the `CookbookPayload` to stdin. The
  `unwrap_or_default()` fallback exists for robustness against bugs (e.g.
  trusted-core failing to populate the payload), not as a supported
  standalone-use path. Plugin authors test through `enw`.

- MVP LOC: ~350 including tests — trusted-core merger, the payload type,
  invocation-site pipes at `enwiro-sdk/src/client.rs:125`, four cookbook
  adoptions, tests.

## Related decisions

Future ADRs likely to follow:

- **CEL expression layer** — when #206 (hooks/templates) lands. TOML strings
  hold expressions; the consumer evaluates them at use-time.
- **Cookbook inheritance** — when #301 lands. Adds `metadata.extends:
  ["scope"]` and a `CookbookPayload.inherited` field carrying parent
  configs as `{ type, name, config }`.
- **`enw config get` / `enw config explain`** — when user pain justifies
  it. Likely triggers a swap from `config-rs` to `figment` for per-key
  provenance.
- **Schema export (`enw config schema`)** — orthogonal; add `schemars` +
  `garde` derives if user-facing schemas become needed.
- **Per-env overrides** (`<env>/overrides.toml`) — another walker target.
- **Trust gating for repo-level exec** — only if a future feature
  introduces script/hook fields. direnv / pnpm content-hash allowlist is
  the obvious pattern.

## References

- Existing user-level config callsites:
  - `enwiro-daemon/src/main.rs:5`
  - `enwiro-cookbook-git/src/main.rs:1199`
  - `enwiro-cookbook-github/src/main.rs:1454`
  - `enwiro-adapter-i3wm/src/main.rs:93`
- Duplicated `GitCookbookConfig` at
  `enwiro-cookbook-github/src/main.rs:21-28` — the cross-cookbook
  data-access problem; solved separately via subprocess composition, out
  of scope here.
- `AdapterPayload` at `enwiro-sdk/src/adapter.rs:17-43`.
- `config-rs`: https://github.com/rust-cli/config-rs
- `config-rs-ng` (no timeline):
  https://github.com/matthiasbeyer/config-rs-ng
- pnpm 10 blocking lifecycle scripts (Jan 2025), an ecosystem precedent for
  "do not autorun on read":
  https://socket.dev/blog/pnpm-10-0-0-blocks-lifecycle-scripts-by-default
- direnv allow content-hash trust model, the obvious pattern if tier-2
  trust gating is later needed: https://direnv.net/man/direnv.1.html
- CEL for the future expression layer: https://cel.dev
- Issue #379 — "understand config system needs."
