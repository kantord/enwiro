# ADR-0002: Daemon IPC architecture

## Status

Proposed

## Context

Multiple open issues demand a coherent story for how processes in the enwiro
ecosystem talk to each other:

- **#432** asks the daemon to expose env-activation events to external
  listeners (costae, AI surfaces, generic).
- **#278** ("rely on unix sockets to connect to daemon instead of files") is
  the umbrella IPC issue, currently `Ready`. #432 is one of its
  consumer-facing surfaces; #357 (`Depends on #278`) is another.
- **#297** (costae flash on swap), **#298** (context recap), **#302** (live
  status tracking), **#348/#386** (AI surfaces), **#395** (agent
  coordination), **#436** (plugin discovery TUI) all want some flavour of
  pushed events or queried state from the daemon.
- **#301** ("formalize extension/composition rules for recipes/cookbooks")
  wants cookbook composition; the user has since refined the framing toward
  cookbook-as-client of the daemon rather than `enw`-orchestrated chaining.
- **#427/#428/#431** add new lifecycle hooks (user-defined, Cook, Destroy)
  whose firings need to be observable.

Today's communication shapes:

- **Adapter → daemon** — the daemon `Command::spawn`s the adapter's
  `listen` subcommand and reads stdout as JSONL workspace-switch events
  (`enwiro-daemon/src/lib.rs:263-319` + `enwiro-adapter-i3wm/src/main.rs:550-587`).
- **Daemon → enw** — the daemon writes `recipes.cache` to
  `$XDG_RUNTIME_DIR/enwiro/`; `enw` reads it via
  `DaemonCache::read_recipes()` (`enwiro-daemon/src/lib.rs:36`,
  consumed at `enwiro/src/context.rs:99-103` and
  `enwiro/src/commands/ls.rs:144-151`). `enw` still re-implements the
  cookbook iteration / sort / filter logic on top of the cached recipes.
- **Host → cookbook plugin** — host invokes
  `<plugin> list-recipes` / `<plugin> cook` and parses stdout JSON
  (`enwiro-sdk/src/client.rs`, `CookbookClient::list_recipes`).
- **External listeners** — none. costae derives env state indirectly from
  i3 workspace events (`costae-i3/src/main.rs`) and
  `/proc/<pid>/environ` lookups (`costae-notify/src/server.rs:81`,
  `read_enwiro_env`); there is no direct enwiro IPC channel for any external
  consumer.

The plugin contract that already exists (cookbook stdout JSON, adapter
listen stdout JSONL) maps directly onto tauler's `useJSONStream(bin)`
pattern at `/home/kantord/repos/tauler/src/jsx.rs:45`, which is used in
production by the user's chezmoi scripts (e.g.
`~/.local/share/chezmoi/dot_local/bin/executable_tauler-devcontainers` —
plain bash emitting JSON to stdout when state changes). The design goal
that *"a bash script could be an enwiro cookbook or garnish or adapter"*
hinges on keeping that interface trivial.

What is missing today is a single, coherent IPC story for **clients of the
daemon**: `enw`, future external apps, and (per the refined #301 framing)
cookbook binaries themselves. Today's file-based cache + ad-hoc fork
patterns work but force every client to re-implement business logic that
should live in the daemon.

## Decision drivers

- **Two distinct concerns, two distinct contracts.** The interface for a
  publisher emitting data ("here is the recipe list") and the interface for
  a client querying state or subscribing to events ("what env am I in?",
  "tell me when env changes") have different ergonomic needs and should
  not be conflated.
- **Plugin interfaces must stay trivially simple.** A bash script with a
  `printf` of JSON to stdout should remain a viable plugin. Boilerplate
  belongs on the host side, not in plugin authors' code.
- **`enw` is a client, not a state owner.** Daemon owns business logic and
  state; `enw` calls into it. Daemon is the single source of truth and
  logic.
- **Shell-discoverable wire format.** The dominant positioning frame for
  enwiro is *"so modular that it's basically almost a glorified wire
  format"*. The chosen protocol must remain debuggable with `socat`/`nc` +
  `jq`, with no extra tooling required.
- **`useJSONStream` compatibility.** tauler panels (and similar lightweight
  consumers) spawn a binary and parse its stdout as JSON. The IPC story
  must preserve a path for these consumers.
- **Forward-compatibility without bumping protocol versions for every
  field.** Unknown fields and unknown event kinds must be ignored, not
  errored on.
- **Multi-client without `enw` as a bottleneck.** Other apps (costae,
  tauler, future tooling) should be able to connect to the daemon directly,
  not only through `enw`.
- **Cookbook delegation matters.** Refined #301 framing: cookbook A asks
  the daemon to invoke cookbook B and add layered logic on top. Whatever
  RPC mechanism the clients use must also accept calls from cookbook child
  processes.
- **Local-only is fine for now.** Single-user, single-host. No
  authentication at the IPC layer; the socket's filesystem perms
  (`0600` inside `$XDG_RUNTIME_DIR` mode `0700`) are the entire trust
  story. External auth layers can wrap if a network use case ever arrives.
- **No new infrastructure without a confirmed consumer.** Real-time event
  push (Layer 3 below) has zero built consumers today; design it, don't
  implement it yet.

## Considered options

### Architecture shape

- ✓ **Chosen — Two-channel split.** Publishers (cookbook, adapter, future
  garnish/gear) keep using stdout-JSONL. Clients (`enw`, costae, tauler,
  cookbooks-as-clients, future external apps) use a typed RPC over UDS.
  Each side optimised independently.
- ✗ **Rejected — Single-channel stdout-JSONL for everything.** Forcing
  clients into fork-per-query doesn't scale to many call sites, persistent
  subscriptions, or non-`enw` clients connecting to live daemon state.
- ✗ **Rejected — Migrate adapters/cookbooks to UDS too.** Pattern is
  intentional and matches tauler `useJSONStream` ergonomics; moving
  publishers to UDS adds boilerplate that contradicts the "bash script is
  a viable plugin" goal.

### Client↔daemon transport

- ✓ **Chosen — JSON-RPC 2.0 over UDS.** Plain JSON envelope on the wire
  (`{"jsonrpc":"2.0","id":..,"method":..,"params":..}` →
  `{"jsonrpc":"2.0","id":..,"result":..}`), `id`-less server-initiated
  `events.notify` notifications for streaming, types defined as shared
  serde structs in `enwiro-sdk`. Debuggable with `socat - UNIX-CONNECT:... | jq`;
  degrades gracefully into "just JSON" for consumers that don't know the
  spec. JSON-RPC libraries exist for every major language; hand-rolling a
  client is also trivial.
- ✗ **Rejected — gRPC over UDS via tonic.** Strongest static typing
  (protobuf) and native server-streaming RPCs, but ~150 transitive crates,
  `protoc` in CI, `tonic-build` in `build.rs`, and a binary wire that needs
  `grpcurl` + server reflection or a `.proto` file to inspect. Hostile to
  shell ergonomics and the `useJSONStream` consumer pattern. Polyglot
  codegen is excellent for Python/Go/TS but unusable from bash without an
  extra tool. Reconsider only if a polyglot client demands stronger typing
  than JSON-RPC + JSON Schema.
- ✗ **Rejected — Cap'n Proto RPC.** Binary wire (same `jq`-debug loss),
  niche tooling.
- ✗ **Rejected — MessagePack-RPC.** Binary wire, same trade-off.
- ✗ **Rejected — Hand-rolled enum + `serde_json` framing.** Re-invents
  JSON-RPC (ids, errors, notifications, cancellation) with extra steps.

### Where shell-wrapper subcommands live

- ✓ **Chosen — On `enw`** (`enw current-env`, `enw events tail`,
  `enw list-recipes`). Matches the dominant **unified-client-CLI** pattern
  in the wild: Docker (`docker` vs `dockerd`), Nix (`nix` vs
  `nix-daemon`), Kubernetes (`kubectl` vs server binaries). Aligns with
  `enw`'s target state as a thin client: every `enw` subcommand becomes
  one or a small composition of RPC calls.
- ✗ **Rejected — On `enwiro-daemon`.** Mixes server and client concerns
  in one binary. systemd does this with multiple specialised client
  utilities (`systemctl`, `journalctl`, `loginctl`), but only because of
  surface complexity enwiro doesn't have.
- ✗ **Rejected — Separate `enwiro-events` / `enwiro-current` binaries.**
  PostgreSQL-style split clients are justified by broad surface; enwiro's
  surface is narrow.

### Cookbook composition (refined #301)

- ✓ **Chosen — Cookbooks call the daemon over RPC** to delegate to other
  cookbooks ("cookbook A asks daemon to ask cookbook B to cook recipe R,
  then A adds Y on top"). `cookbook.invoke(cookbook, op, args)` becomes
  the orchestration primitive. Cookbooks discover the RPC socket via
  `$ENWIRO_RPC_SOCKET` set in the child process env by the daemon at spawn
  time.
- ✗ **Rejected — Direct cookbook → cookbook calls.** Would push OOP-style
  inheritance/dispatch into the cookbook protocol; daemon would have no
  visibility for cycle detection, audit, or future policy.
- ✗ **Rejected — Pure host-orchestrated chaining (the strict #301
  framing).** `enw` runs recipes in order with no cookbook-side
  participation. Workable but inflexible; cookbook A can't depend on
  cookbook B's *output* (e.g. worktree path) without an out-of-band
  convention. Cookbook-as-client cleanly resolves this.

### Event-stream implementation timing

- ✓ **Chosen — Design now, implement when first consumer ships.** Add
  `events.subscribe` / `events.notify` to the JSON-RPC surface in this
  ADR. Do not write the daemon-side broadcast plumbing until #297 (costae
  flash) or another confirmed consumer needs it.
- ✗ **Rejected — Implement now anyway.** Speculative; no built consumer.
- ✗ **Rejected — Defer the design too.** Forces the next ADR to invent the
  shape from scratch; risks divergent vocabulary across event kinds.

## Decision

1. **Two-channel IPC architecture.** Pick the transport by direction of
   data flow:

   - **Plugin → host channel** stays stdout-JSONL. Cookbook `list-recipes`,
     cookbook `cook`, adapter `listen`, future garnish/gear data emission
     all keep the existing contract: invoke as a subprocess, parse JSON
     lines from stdout. Bash scripts remain viable plugins. No change in
     this branch.
   - **Client ↔ daemon channel** = **JSON-RPC 2.0 over UDS** at
     `$XDG_RUNTIME_DIR/enwiro/rpc.sock` (perms `0600`). Used by `enw`,
     external clients (costae, tauler, future apps), and **cookbook
     binaries during their execution** (for `cookbook.invoke` delegation).

2. **Daemon binary stays server-only.** `enwiro-daemon` runs the RPC
   server and the existing cache-refresh / adapter-listen loops; it
   exposes no user-facing subcommands. `enw` is the unified client CLI
   (docker/nix pattern). Every `enw` subcommand maps to one or a small
   composition of RPC calls.

3. **Shell-wrapper subcommands on `enw`.** `enw current-env`,
   `enw events tail`, `enw list-recipes` each make one RPC call and print
   JSON to stdout, preserving `useJSONStream` and shell-pipeline
   compatibility for consumers that don't link an RPC client library.
   Wrappers MUST emit only the **inner result/event payload**, not the
   JSON-RPC envelope. Concretely: `enw current-env` prints
   `{"v":1,"env":"...","recipe_id":"...","ts":"..."}` (one line);
   `enw events tail` prints one inner event per line as the daemon emits
   them (`{"v":1,"kind":"env_activated","ts":"...","env":"...",...}`),
   stripping the surrounding `{"jsonrpc":"2.0","method":"events.notify","params":{"subscription_id":...,"event":...}}`
   envelope. `useJSONStream("enw current-env")` and
   `useJSONStream("enw events tail")` both work without unwrapping.

4. **Cookbook-as-client.** Any cookbook process running on the system,
   spawned by the daemon **or by a developer at the shell**, gets
   `ENWIRO_RPC_SOCKET=$XDG_RUNTIME_DIR/enwiro/rpc.sock` in its env iff
   the daemon is running and the socket exists. Concretely: the daemon
   exports the var into its own process env at startup; child processes
   it spawns inherit it; a developer-invoked cookbook also sees it
   because `$XDG_RUNTIME_DIR` is the same per-user dir. The rule is
   *"socket exists ⇒ env var present"*, not *"daemon-dispatched ⇒ env var
   present"*. This avoids the foot-gun where cookbook code behaves
   differently depending on whether the daemon or the shell spawned it.

   Cookbooks may open the socket and call
   `cookbook.invoke(cookbook, op, args)` to delegate work to another
   cookbook via the daemon. The daemon dispatches the requested operation
   by spawning the named cookbook with the existing stdout-JSONL protocol
   and returns the result over RPC.

   **Cycle detection** is tracked transitively across the spawn tree
   (not per-RPC-connection, because each delegated cookbook child opens
   its own connection). The daemon sets
   `ENWIRO_RPC_CALL_CHAIN=<colon-separated-cookbook-names>` in the env
   of any cookbook process it spawns via `cookbook.invoke`. When a
   cookbook calls `cookbook.invoke`, the client SHOULD include its
   inherited chain in `params.call_chain` (the SDK helper does this
   automatically); the daemon refuses to extend a chain that would
   repeat a cookbook name. Cookbook authors writing raw clients without
   the SDK helper risk losing cycle protection — documented and
   accepted; the SDK is the supported path.

5. **`cookbook.invoke` is the pilot.** The first concrete RPC method to
   implement (after the bare minimum server skeleton) is
   `cookbook.invoke`. It exercises every layer this ADR commits to
   (socket bind, JSON-RPC framing, method dispatch, cookbook subprocess
   spawn from inside an RPC handler, typed result) and resolves the
   refined #301 use case with a real consumer.

6. **RPC envelope.**

   - Request:
     ```json
     {"jsonrpc":"2.0","id":N,"method":"<namespace>.<method>","params":{...}}
     ```
   - Response (success):
     ```json
     {"jsonrpc":"2.0","id":N,"result":{"v":1,...}}
     ```
   - Response (error):
     ```json
     {"jsonrpc":"2.0","id":N,"error":{"code":-32xxx,"message":"...","data":{...}}}
     ```
   - Server-initiated notification (for events):
     ```json
     {"jsonrpc":"2.0","method":"events.notify","params":{"subscription_id":"...","event":{"v":1,"kind":"...","ts":"...",...}}}
     ```
   - Framing: one JSON object per line (newline-delimited). Newlines inside
     JSON string values are escaped per RFC 8259; no embedded literal
     newlines.

7. **Method namespaces (initial; expand additively).** All names use
   `<namespace>.<method>` dotted form. Single-noun methods are reserved
   for protocol-level concerns (`health`); everything else lives in a
   namespace.

   - `env.current` — returns the active env. Daemon serves this from an
     in-memory "last activated env" state that it populates from the
     adapter's `workspace_switch` event stream (the same callback site
     today: `on_workspace_switch` at `enwiro-daemon/src/lib.rs:308-314`).
     If the daemon has not yet seen a switch event since start, returns
     an explicit "unknown" result rather than guessing from
     `/proc/<pid>/environ`.
   - `recipes.list` — list cached recipes (optionally filtered by cookbook).
   - `cookbook.invoke` — delegate to another cookbook (pilot method).
   - `status.get` — live env status, when #302 lands.
   - `cache.status` — per-cookbook freshness, when #357 lands.
   - `health` — daemon liveness and protocol-version probe. Single noun;
     deliberately unnamespaced because it predates any namespace.
   - `events.subscribe` / `events.notify` / `events.unsubscribe` —
     designed; implementation deferred to the first event consumer.

8. **Event taxonomy (designed; ship `env_activated` first).** Reserved
   kinds with `kind`-discriminated payloads:

   - `env_activated` — emitted on env swap; payload includes `env`,
     `recipe_id`, `prev_env`, `worktree_path`, `source`, `ts`.
   - `env_cooked` — emitted after a cook completes.
   - `env_destroyed` — emitted by `enw rm` (#337) once #431
     (`Hook::Destroy`) lands.
   - `hook_fired` — covers built-in (`Cook`, `Destroy`) and user-defined
     hooks (#427/#428). Payload: `{ "env": "...", "hook": "<name>",
     "source": "auto|manual" }`.
   - `status_changed` — Active / Inactive / Evergreen per #302.
   - `cache_refreshed` — per-cookbook cache refresh, per #357.

9. **Versioning and compatibility.** Every result payload and every event
   body carries a `v` field (start at `1`). **Scope of `v` is per-shape,
   not daemon-global**: each method's result shape and each event `kind`
   has its own independent `v` counter. The daemon advertises its own
   build version separately via `health` (this is *not* the same as
   payload `v`; clients SHOULD NOT use the daemon build version for
   compatibility decisions).

   Adding new fields to an existing shape does not bump that shape's
   `v`. Bumping `v` is reserved for *breaking* changes within a single
   payload shape (e.g. removing a field, changing semantics). Adding
   new methods is non-breaking by definition. The protocol is
   **unknown-field-ignored** and **unknown-method-errored** (JSON-RPC
   standard `-32601 Method not found`); event consumers MUST ignore
   unknown `kind` values silently.

10. **No authentication at the IPC layer.** Single-user assumption holds;
    the socket lives at perms `0600` in `$XDG_RUNTIME_DIR` (mode `0700`).
    The hook-execution safety story from #428 (untrusted cookbook envs
    must not trigger user automations) lives at the hook-execution layer,
    not here.

11. **No public socket contract beyond this ADR.** Cookbooks see
    `ENWIRO_RPC_SOCKET` and the JSON-RPC envelope; that's the contract.
    Internal daemon refactors (move state machines around, swap the
    runtime, add a thread pool) do not need a new ADR as long as the
    wire shape and method set remain compatible.

12. **Resolves #278; designs #432 (impl tracked separately).** This ADR
    is the resolution of #278: the unix socket connecting clients to the
    daemon is the JSON-RPC surface described above. #432 ("expose
    env-activation events to external listeners") is *designed* here
    via `events.subscribe` + `events.notify` + the `enw events tail`
    wrapper, but is **not** implemented in this branch and SHOULD NOT
    be closed on ADR merge. #432 closes when the first event consumer
    (likely #297) ships against this interface.

13. **Refines #301.** Cookbook composition is no longer pure
    host-orchestrated chaining; it's daemon-mediated delegation via
    `cookbook.invoke`. #301 should be updated (or superseded by a
    follow-up issue) to reflect this.

## Consequences

### Positive

- **Single source of truth and logic in the daemon.** `enw`, costae,
  tauler, and future clients all consult one place. Drift between
  re-implementations becomes impossible by construction.
- **`enw` thinning is unlocked.** Each `enw` subcommand can migrate to a
  small RPC composition in a self-contained PR. The work happens
  incrementally without a flag-day rewrite.
- **Non-`enw` clients become first-class.** costae's flash (#297), AI
  surfaces (#348/#386), tauler panels (`useJSONStream("enw events tail")`),
  TUIs (#436) all connect via the same protocol.
- **Cookbook composition has a real home.** `cookbook.invoke` lets the
  github cookbook delegate to git rather than re-declaring the git
  cookbook's config schema. The duplicated `GitCookbookConfig` block at
  `enwiro-cookbook-github/src/main.rs:26-29` (referenced in ADR-0001's
  references section) is the concrete mirror that becomes
  unnecessary once `cookbook.invoke` exists. The broader #301
  motivation — composing cookbooks without out-of-band conventions —
  follows from the same primitive.
- **Shell-discoverable wire format.** `socat - UNIX-CONNECT:... | jq`
  works at every step. Bash-only consumers remain viable, both as
  publishers (existing pattern) and as consumers (via `enw <wrapper>`
  subcommands).
- **`useJSONStream` compatibility is preserved.**
  `useJSONStream("enw events tail")` slots into tauler panels with no
  changes; the wrapper strips JSON-RPC envelopes and emits the inner
  event payload one per line.
- **Schema evolution is cheap.** Additive fields don't bump `v`; unknown
  kinds are silently dropped by consumers. Forward and backward compat
  fall out of the convention.
- **#357 is unblocked.** Per-source freshness becomes a normal
  daemon-internal feature plus a `cache.status` RPC, with no protocol
  redesign needed.
- **Layer 3 events are designed but not implemented.** Builders of #297
  and similar have a contract to target without paying implementation
  cost up front.

### Negative / Trade-offs

- **One more piece of infrastructure to keep alive.** The JSON-RPC server
  inside the daemon must be robust (panic-safe, backpressure-handled,
  socket cleanup on restart). Mitigated by JSON-RPC's small surface and
  the existing daemon-as-required posture (#330).
- **Two protocols to maintain.** Stdout-JSONL upstream and JSON-RPC
  downstream are distinct. Acceptable: they're aligned semantically
  (`kind`-discriminated payloads, `v`-versioned, unknown-tolerant) and
  differ only in framing. Both schemas live in `enwiro-sdk`.
- **JSON-RPC typing is "in code", not "on the wire".** Non-Rust clients
  re-derive types from the shared definitions (or work from a published
  JSON Schema). Trade-off accepted because protobuf's costs are larger
  than its typing benefit at our current scale.
- **Daemon-down failure mode becomes user-visible.** `enw` exits with a
  JSON-RPC error if the socket isn't bindable. Mitigated by systemd unit
  posture (`Restart=always`, per #330's spirit); document the failure
  message so users can diagnose.
- **Migration cost.** Existing `enw` code (e.g. `context.rs:99-103`,
  `commands/ls.rs:144-151`) needs to migrate from `DaemonCache`
  file-reads to RPC calls over time. Incremental; tracked separately.

### Risks

- **Subscription backpressure stalls the daemon.** Mitigation: drop and
  disconnect slow subscribers with a logged error rather than block the
  event broadcast.
- **Recursive `cookbook.invoke` cycles.** Per-RPC-connection tracking is
  insufficient because each delegated cookbook child opens its own
  connection. Mitigation: tracking is transitive across the spawn tree,
  carried via `ENWIRO_RPC_CALL_CHAIN` env var (see Decision §4). Daemon
  refuses to extend a chain that would repeat a cookbook name, returning
  a `-32xxx` error with the offending chain. Cookbook clients written
  without the SDK helper may forward an empty chain and lose cycle
  protection — documented trade-off; the SDK helper is the supported
  path.
- **`enw` <-> daemon version skew.** Mitigation: `health` RPC reports
  both daemon build version and the protocol version (the latter is the
  highest payload-shape `v` the daemon understands). `enw` warns (not
  errors) when the daemon's protocol version is older than the one
  `enw` was built for. The unknown-field-ignored rule means most skew
  is silent.
- **Schema sprawl.** Mitigation: a single `enwiro-sdk::rpc` module owns
  every method shape. New methods land with their request/response
  structs in one PR.
- **Authz becomes relevant later.** Today's "local-only, socket perms are
  enough" stance fails if a network use case lands (#380). Mitigation:
  any future network transport is a separate ADR; until then, the
  socket-perms story stands.

## Implementation notes

### Scope of *this* branch

- This ADR only. No code lands here. The decisions above are
  prerequisites for follow-up branches.

### First follow-up branch (the pilot)

- Add a tokio-based JSON-RPC 2.0 server to `enwiro-daemon`. Use an
  existing crate (`jsonrpc-core`, `jsonrpsee`, or a small in-house
  newline-delimited dispatcher). LOC estimate: ~300 including tests.
- Add the `enwiro-sdk::rpc` module with shared request/response types.
  Initial methods: `health`, `env.current`, `cookbook.invoke`. The SDK
  helper for `cookbook.invoke` reads `ENWIRO_RPC_CALL_CHAIN` from env
  and forwards it in `params.call_chain` automatically.
- Implement `cookbook.invoke` end-to-end. Daemon resolves
  `cookbook` → plugin path, spawns it with the existing stdout-JSONL
  protocol, returns the typed result.
- Set `ENWIRO_RPC_SOCKET` in the cookbook child env when the daemon
  spawns one. Document the convention in the cookbook protocol docs.
- Migrate one cookbook (likely `enwiro-cookbook-github`) to call
  `cookbook.invoke({"cookbook": "git", ...})` instead of duplicating
  git logic, demonstrating the pattern.

### Subsequent follow-ups (one PR each)

- `recipes.list` RPC, `enw list-recipes` wrapper, migrate
  `enwiro/src/commands/ls.rs` off `DaemonCache::read_recipes()`.
- `events.subscribe` / `events.notify` implementation, gated on first
  consumer (#297 costae flash is the leading candidate).
- `status.get` + `status_changed` events for #302.
- `cache.status` + `cache_refreshed` events for #357.
- `env.current` RPC + `enw current-env` wrapper + remove last
  `DaemonCache` direct read in `enwiro/src/context.rs:99-103`. Daemon
  gains an in-memory `current_env` state populated by the existing
  adapter-stream callback at `enwiro-daemon/src/lib.rs:308-314`.

### Touchpoints

- `enwiro-daemon/src/main.rs` — add `serve_rpc()` alongside the existing
  `enwiro_daemon::run(...)` call.
- `enwiro-daemon/src/lib.rs:31` — `DaemonCache` shrinks over time as
  callers migrate to RPC; eventually removable.
- `enwiro-sdk/src/lib.rs` — add `pub mod rpc;` with request/response
  structs and an envelope helper.
- `enwiro/src/main.rs` — add wrapper subcommands (`current-env`,
  `events`, etc.) under the existing `EnwiroCli` enum.
- `enwiro-cookbook-github/src/main.rs:26-29` — site of the pilot
  migration; the duplicated `GitCookbookConfig` block can begin to
  collapse.

### Gotchas

- **`$XDG_RUNTIME_DIR` wiped on logout.** Daemon must `mkdir -p` the
  enwiro subdirectory on startup and `unlink` any stale socket file
  before `bind`.
- **`ENWIRO_RPC_SOCKET` presence rule** (see Decision §4): the var is
  set by the daemon in its own process env at startup; child processes
  it spawns inherit it; a developer running a cookbook from a shell
  inside the same user session also sees it because the socket lives at
  the well-known per-user path. *Presence implies the socket should
  exist*; absence implies no running daemon, and cookbooks SHOULD
  degrade gracefully (the `unwrap_or_default()` posture from ADR-0001
  applies). Cookbooks MUST NOT branch on "who spawned me" — the
  contract is socket-existence, not spawn-source.
- **Subscription IDs are scoped to a connection.** On disconnect the
  daemon drops every subscription belonging to that connection. No
  explicit unsubscribe is needed for hard exits; an
  `events.unsubscribe(id)` method handles graceful cases.
- **JSON-RPC notifications (no `id`) are fire-and-forget.** Events use
  notifications; clients cannot ACK individual events. This is the
  intentional semantic.

## Related decisions

- **ADR-0001** (`docs/adr/0001-project-level-config.md`) — defines the
  cookbook protocol payload shape. ADR-0002 extends the cookbook
  contract by adding an env var (`ENWIRO_RPC_SOCKET`) and an optional
  outbound capability (calling `cookbook.invoke`). Non-breaking.
- **#278** — resolved by this ADR.
- **#432** — *designed* by this ADR (`events.subscribe` + `events.notify`
  + `enw events tail` wrapper); implementation tracked separately,
  gated on the first concrete consumer (likely #297). Do not close on
  ADR merge.
- **#301** — refined; will be updated (or superseded by a follow-up
  issue) to reflect daemon-mediated cookbook delegation.
- **#357** — unblocked.
- **#302, #297, #298, #348/#386, #395, #427/#428, #431, #436** — all
  gain a defined consumer interface.
- **#380 (remote environments)** — future ADR. The three shapes the user
  identified (local daemon to remote env directly; local daemon to
  remote daemon bridge; "remote env" = mounted files, no remote logic)
  do not require a heavier protocol than JSON-RPC. None forces a
  redesign of this ADR's surface; a network transport (TCP+TLS) would
  layer on top of the same JSON-RPC envelope.
- **Future "enw config explain" ADR** — orthogonal; lives in ADR-0001's
  consequences list.

## References

- `enwiro-daemon/src/lib.rs:263-319` and `enwiro-adapter-i3wm/src/main.rs:550-587`
  — existing adapter→daemon JSONL-on-stdout pattern. Kept as the
  publisher channel.
- `enwiro-daemon/src/lib.rs:36` `pub struct DaemonCache` (and
  `read_recipes` at `lib.rs:58`) plus call sites at
  `enwiro/src/context.rs:99-103`, `enwiro/src/commands/ls.rs:144-151`
  — today's `enw ↔ daemon` coupling via file reads; will migrate to RPC.
- `enwiro-sdk/src/cookbook.rs` and `enwiro-sdk/src/client.rs`
  (`CookbookClient::list_recipes`) — cookbook protocol; gains
  `ENWIRO_RPC_SOCKET` + optional `cookbook.invoke` outbound capability.
- `costae-i3/src/main.rs`, `costae-notify/src/server.rs:81`
  (`read_enwiro_env`) — costae's current indirect env-state derivation
  via i3 events + `/proc/<pid>/environ`. The first listener interface
  for #297 will replace these for the swap-flash use case.
- `/home/kantord/repos/tauler/src/jsx.rs:45` —
  `globalThis.useJSONStream = (bin, script) => ...`. The consumer
  pattern that `enw events tail` preserves.
- `~/.local/share/chezmoi/dot_local/bin/executable_tauler-devcontainers`
  and siblings — bash scripts emitting JSON to stdout. Proof that the
  publisher contract works with zero framework.
- JSON-RPC 2.0 specification — https://www.jsonrpc.org/specification.
- Docker daemon/client split — `dockerd` vs `docker`.
- Nix daemon/client split — `nix-daemon` vs `nix`.
- Kubernetes client CLI — `kubectl`.
- Issue #278 — *"rely on unix sockets to connect to daemon instead of files"*.
- Issue #432 — *"expose env-activation events to external listeners"*.
- Issue #301 — *"formalize extension/composition rules for recipes/cookbooks"*.
