# Streaming render performance

How Waku keeps CPU flat while a provider streams, what each piece of the
pipeline is allowed to cost, and how to measure before changing any of it.
This encodes the results of the 2026-08-16 streaming investigation, which took
sustained streaming CPU from 40–60% to under ~10% average (debug build) across
text and fast-reasoning streams.

The one-line model: **CPU ≈ redraw rate × visible element count.** GPUI
rebuilds and lays out every visible element on every frame a view renders —
[`list()`](https://github.com/egoist/zed) re-renders and `layout_as_root`s
every visible row per frame, cached heights only spare the overdraw — so every
rule below either bounds how often frames happen or how much is visible inside
one.

## Who is allowed to cause a frame

A frame happens when an entity is notified, when `window.refresh()` is called,
or when something re-arms `request_animation_frame`. Each has a different
price:

| Trigger | Cost | Allowed users |
| --- | --- | --- |
| `cx.notify(view)` | Re-renders that view and its ancestors; **cached sibling panes replay** | The stream pump (per commit), the pulse clock, user-event handlers |
| `window.refresh()` | Re-renders everything and **bypasses every cached pane** | Genuine whole-window invalidation only: hover transitions, drags, theme |
| `request_animation_frame` | Display-rate (120 Hz) re-render of the current view for as long as it re-arms | Nothing during streaming. One mounted repeating `with_animation` pinned the window at 120 Hz for a whole turn (~36% CPU by itself). The one sanctioned transient: the 200 ms panel show/hide slide ([src/app/render.rs](../src/app/render.rs)), which re-arms only while an edge is moving and gates the pane fan-out (below) |

The root `Waku` view re-renders on every frame regardless of what is dirty, so
it must stay thin: the sidebar, transcript, and right panel are `WakuPane`
islands ([src/app.rs](../src/app.rs)) embedded with the fork's
`Entity::cached`. Each pane observes the root — any root notify still
re-renders every island, so caching can never show stale state — while a
notify targeted at one pane (the pulse clock leases `window.current_view()`)
rebuilds only that island and replays the rest.

The one exception to that fan-out is the 200 ms panel slide: its display-rate
root notifies would price every tick at a three-island rebuild, so while
`panels_sliding()` the observer skips the fan-out and the cached-view keys
decide instead — the sliding panel (its clip moves) and the transcript (its
bounds move) miss their caches and rebuild with fresh state anyway, while the
island nothing is moving replays. Updates born inside an island still land
mid-slide because a child notify dirties its ancestor pane without the
observer, and the slide's retirement schedules one ungated notify so any
root-state drift in a reused pane converges the frame after the slide ends. Two traps to know: gpui's
`mark_view_dirty` walks **ancestors only**, which is why panes must observe
the root rather than expect root notifies to reach them; and a cached pane
lays its content out **as a root**, so a `flex_1`-sized subtree collapses to
its zero flex basis without a `size_full` flex wrapper.

## The two cadences

Everything during a stream happens at one of two rates, and every change to
the pipeline must keep it that way:

**Commits, ≤ ~8.3 Hz.** Provider chunks queue for a full
`STREAM_FRAME_INTERVAL` (120 ms, matching Zeron's `STREAM_COMMIT_MS`) and fold
into one drain → one notify → one tail remeasure
([src/app.rs](../src/app.rs), [src/app/runtime.rs](../src/app/runtime.rs)).
Two hard-won rules:

- The pump timer must **not** race the wake channel. It used to, which made
  the notify rate equal the provider's chunk rate.
- Every *streaming* delta kind must set the flags that route the pump onto the
  `StreamFrame` schedule. `ReasoningDelta` originally did not set
  `markdown_changed`, so a reasoning-only drain reported `Idle`, the pump went
  back to sleeping on the wake channel, and every fast thinking chunk woke it
  for an immediate drain-and-notify — **40+ commits and full re-renders per
  second**, sailing straight past the floor. Fast thinking hitting 40% CPU
  while text streamed at 10% was this one flag.

**Pulse ticks, ≤ ~30 Hz.** All repeating animation rides the shared
self-parking clock in [src/ui/motion.rs](../src/ui/motion.rs) (ported from
Zeron): loaders read a phase from a shared epoch, leases expire 300 ms after
the loader last painted, and the clock parks when no leases remain. Never use
`with_animation(...).repeat()` — it re-arms `request_animation_frame` every
display frame. A view's whole subtree rebuilds per tick, so cadence is priced
per *view*, not per animation: leases carry a stride (`spin_slow`,
`pulse_lease_slow`, `Pulse::every(2)` ≈ 15 Hz) for loaders mounted on
expensive surfaces — the working dots set the transcript pane's tick floor for
the entire turn. Strides re-establish on every tick (a lease's stride resets
after it fires); an earlier version kept the minimum stride forever, so one
full-rate lease permanently dragged its pane back to 30 Hz.

The veil dissolve is a pulse-clock client like everything else: the message
veil at full rate, the reasoning veil strided, both leasing
`window.current_view()` so a dissolve only rebuilds the island that hosts it.

**Overlay scrollbars are the classic violator of both cadences.** A streaming
surface moves its content every commit, so the bar sits in its reveal hold for
the whole turn — and the hold is constant-opacity, needing zero repaints.
[src/ui/scrollbar.rs](../src/ui/scrollbar.rs) therefore schedules a single
one-shot wake for hold expiry and rides the pulse clock only through the
350 ms fade. Driving frames through the hold pinned the pane at pulse rate the
moment any scrollbar became visible.

## Bounding what is visible

- The transcript is virtualized with `list()`; per-commit invalidation is the
  last `STREAM_REMEASURE_TAIL_ROWS` rows only, and row folds, navigation
  turns, response footers, and sidebar rows are all fingerprint-cached
  ([src/app/transcript.rs](../src/app/transcript.rs),
  [src/app/sidebar.rs](../src/app/sidebar.rs)). A fingerprint must hash at
  display granularity: the sidebar row cache keys session recency, and hashing
  raw seconds would bust it on every commit.
- The live reasoning peek renders a **byte window** of the tail
  (`live_reasoning_window_start`,
  [src/app/transcript_view.rs](../src/app/transcript_view.rs)): markdown cost
  is O(rendered source) per tick regardless of block shape — a wall-of-text
  think is one giant paragraph and a bulleted think one giant list, so a
  block-count cap bounds neither. The slide hysteresis is wide
  (`LIVE_REASONING_WINDOW_MAX`) because fast reasoning appends several KB per
  commit and each slide rebuilds the window from a fresh view. The full trace
  renders once the turn settles.
- `markdown_tail` and block-index element ordinals
  (`block_ix << 16 | position`, [src/md/render.rs](../src/md/render.rs)) let
  a capped walk hand settled blocks the same flatten-cache and veil keys as a
  full walk.
- `MarkdownView::set_text` derives the mended display tail only when content
  or the streaming flag changed — the derivation re-parses the final block and
  runs for every visible row every frame.

## Measuring

Sampling alone misled this investigation for hours; counters cracked it in one
run. In order of usefulness:

1. **Per-second counters, logged from render.** Static `AtomicU32`s counting
   window frames, per-pane renders, and pump commits, flushed once per second
   to a file from the root render (write on the background executor). One
   streaming run gives the full decomposition — it is how the 40-commits/sec
   reasoning bug and the stuck-stride bug were found after profiles showed
   nothing but generic layout work. Wire it temporarily; do not ship it.
2. **`sample <pid> 5` during a captured stream**, `awk '/Sort by top of
   stack/,0'` for leaves, ancestor-walk for the hot chain. Good for *what*
   is expensive (taffy vs shaping vs app code), useless for *how often*.
   The production binary is stripped — profile the debug build.
3. **CPU traces across a whole turn**: poll `ps -o %cpu= -p <pid>` every
   500 ms from trigger until settle. Averages over a turn hide phase
   plateaus; report both.
4. The built-in FPS counter pins the window at display rate by design and
   cannot measure streaming cadence.

Debug-build numbers overweight taffy/style/scene generics by several fold;
treat them as structure, not as what users feel, and confirm user-facing
claims on a release build.

## Known floor and next levers

With both cadences enforced, a streaming frame still rebuilds every visible
row (gpui `list()` semantics). If that ever needs to shrink: fork-level cached
list rows need a measure-once extension to `ViewElement` caching (cached views
lay out from style, not content, which breaks the list's measurement as-is);
alternatively fold activities into the virtualized list as block-granularity
rows the way Zeron does. Smaller levers, in memory and unproven: stable
`StyledText` element ids for gpui's per-element layout memo, and the per-row
`Message` clones in the row builder.
