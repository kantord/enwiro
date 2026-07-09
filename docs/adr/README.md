# Architecture Decision Records

Numbered markdown files capturing technical decisions that outlive any single
PR — the *why* kept alongside the code. Each ADR records the context, the
decision, and the trade-offs.

## Index

| ADR                                        | Title                                  | Status   |
| ------------------------------------------ | -------------------------------------- | -------- |
| [0001](0001-project-level-config.md)       | Project-level config via `.enwiro.toml` | Accepted |
| [0002](0002-daemon-ipc-architecture.md)    | Daemon IPC architecture                | Proposed |
| [0003](0003-pattern-recipes.md)            | Pattern recipes for cooking unlisted names | Accepted |

## Status values

- **Proposed** — under discussion; not yet implemented.
- **Accepted** — decision made, implementation in progress or done.
- **Deprecated** — no longer relevant; kept for historical context.
- **Superseded by ADR-NNNN** — replaced by a later decision. The successor
  cites the original.
- **Rejected** — considered but not adopted. Worth keeping when the
  reasoning is useful for future readers.

## Writing a new ADR

1. Copy [`template.md`](template.md) to `NNNN-short-kebab-title.md` (next
   number, four digits).
2. Fill it in. Keep it concrete: name files, cite line numbers, list real
   alternatives. If you don't know something, say so.
3. Land the ADR with (or alongside) the change it documents.
4. Update this index.

## When to write one

- Project-wide policies (e.g. "no autorun from config").
- Foundational technology picks (library, format, protocol).
- Changes to a contract between subsystems (e.g. the cookbook protocol).
- Reversing or superseding an earlier decision.

Skip for routine implementation, bug fixes, minor refactors, or anything
reviewable as a code diff.
