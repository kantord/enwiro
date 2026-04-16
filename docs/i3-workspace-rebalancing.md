# i3 Workspace Rebalancing — Specification

The i3 adapter (`enwiro-adapter-i3wm`) automatically organises workspaces so that
frequently-used environments occupy the most accessible i3 shortcut slots (slots 1–9,
reachable via `mod+1` … `mod+9`).

---

## Scoring

Each managed environment has a **slot score** in `[0, 1]` derived from its frecency
(activation count + recency). Higher score = more frequently/recently used.

Slot desirability is measured by a **discounted gain** (DCG) value:

```
disc(slot) = 1 / log₂(slot + 1)
```

Lower slot numbers have higher disc values (slot 1 is most valuable, disc → 0 as
slot → ∞).

The **net benefit** of swapping two managed envs between slot `lo` (lower, better)
and slot `hi` (higher, worse) is:

```
NB = (score_hi − score_lo) × (disc(lo) − disc(hi)) − stability_threshold
```

A swap fires when `NB > 0` and has the highest NB among all candidates.

---

## Managed vs. unmanaged workspaces

i3 workspaces not managed by enwiro (e.g. bare workspaces named `"1"` or `"2"`)
are **never displaced**. They block compaction just like managed workspaces: a slot
occupied by an unmanaged workspace is treated as taken and is never used as a
compaction target.

Only managed workspaces participate as swap candidates.

---

## Two rebalancing contexts

### 1. Activate path (`enwiro activate <name>`)

Triggered by an explicit user action: the user is asking for a specific environment
right now.

**Guarantees:**

1. **The new env always lands in slots 1–9.**
   If the next free slot is outside the shortcut zone, the new env performs exactly
   **one swap** with the lowest-scored managed env currently in slots 1–9. That env
   moves to the free slot; everything else stays put.

2. **Score-zero envs are handled.**
   A brand-new env with no frecency history (score `0.0`) gets its effective score
   boosted to `min_shortcut_score + ε` for slot placement only. This represents the
   "just activated" recency signal that hasn't been recorded to disk yet. The stored
   frecency score is not changed.

3. **No stability threshold.**
   Because this is an explicit user action there is no thrash risk, so every
   profitable swap fires regardless of how small the disc gain is
   (`stability_threshold = 0.0`).

4. **Loops to convergence.**
   `find_best_move` is called repeatedly until no profitable move remains. In
   practice the score boost ensures at most one swap involving the new env; further
   iterations handle any remaining imbalance among pre-existing workspaces.

5. **Workspace created before renames.**
   The workspace is created at the free slot first so that i3 can process any
   subsequent rename commands that reference it.

6. **Shortcut stability is preserved.**
   Only one swap displaces an env from the shortcut zone per activation. All other
   shortcuts remain at their current slot numbers, preserving muscle memory for
   quick switching.

### 2. Listen loop (`enwiro-adapter-i3wm listen`)

Triggered passively on every workspace-switch event.

**Behaviour:**

- Applies `STABILITY_THRESHOLD = 0.05`: small disc-gain swaps are suppressed,
  preventing churn during normal workspace switching.
- Rate-limited by a debounce window (default 5 min); at most one rebalance per
  window.
- Runs a single `find_best_move` call per window — does not loop to convergence.
  Any remaining imbalance is resolved in the next window.

---

## Edge cases

| Scenario | Behaviour |
|---|---|
| All shortcut slots are unmanaged | New env stays at its free slot; no managed env can be displaced |
| New env's existing score already exceeds the worst shortcut env | Score boost is a no-op; normal DCG placement runs |
| No managed envs exist at all | `find_best_move` returns empty; workspace is created at the free slot |
| Compaction target is unmanaged | Slot is skipped; next truly empty slot is used |

---

## `find_best_move` — function contract

```
find_best_move(slots, max_shortcut_slot, stability_threshold) → [(old_name, new_name)]
```

- Returns at most **one logical move** per call:
  - Compaction (env moves to empty slot): **1 rename pair**
  - Score-swap (two occupied slots exchange envs): **2 rename pairs**
    (lower-slot env moves out first to avoid name collision)
- Returns empty vec if the layout is already at a fixed point
- Compaction never applies `stability_threshold` (moving into an empty slot is
  always beneficial)
- Score-swap applies `stability_threshold` (see NB formula above)
