# A tick applies each move as it makes it

`drain` resolves the queue against an immutable `World` and writes each move it
accepts into an overlay as it makes it, so a cell served later in the same tick
reads the vacancy an earlier move left. The `World` is unchanged for the whole
evaluation and the read lock still covers it, so a host raycast still runs beside
a tick. The edit list and the commit path are unchanged: the drain still emits
`Clear` at the source and `Set` at the destination for every move, and the
changes reach the `World` only in the commit window under the write lock.

The overlay is what carries the tick's changes to the cells the drain has not
served yet. `Field` owns it and answers `material`, `open`, `grain` and
`supports` from it before the `World`, so the rule reads one consistent state
without any mutation of the `World`. A cell the tick filled holds a material and
a cell the tick vacated holds nothing. Last write wins, so a cell sits in one of
the two at most, which is what a cell vacated by one move and filled by another
in the same tick requires. `blocks`, the player's contact test, still reads the
`World`: the contact test runs before the drain, so there is no overlay to read
yet.

A cell that received a grain this tick counts as in flight, so it supports
nothing. That is the one rule the change needed: the Falling granular rule
already reads a grain in flight as no support, and a filled destination is a
grain in flight whether or not the queue holds the cell it came from.
`supports` reads a filled Falling granular cell as no support, the same
condition that already applied to a queued grain, so the grain above a fill
wakes exactly as it does above a cell the queue is holding. The `claims` set and
the requeue-on-loss branch are gone. A destination a moving grain has taken is
simply not open, so the grain that wanted it finds nothing to fall into and
takes the existing settle path, which wakes the cell above it. One rule replaces
the special case, and the order the queue serves in, `(y, x, z)`, is still what
decides a contested destination.

## What this replaces

The drain used to compute every move against one snapshot of the `World` and
apply the edits afterwards, so a grain could only enter a cell that was already
empty when the tick began. A body therefore could not fall as a body. Measured
on the sand ball, 1,097,917 grains, with no cap and the edits applied afterwards:
the first tick moved 12,853 grains, the vertical span grew from 128 to 256 by
tick 128 while the apex grain had not moved once, and every grain was in a
different phase of its fall. The Cell cap made the same scene wrong a second way,
because a moved cell was requeued at its destination one level lower, so one
tick drained the same lowest cells every tick until they landed. Measured over
3000 capped ticks: 6.4% of the ball's grains were ever evaluated while 93.6%
waited.

Both failures are one failure. A tick's `World` was not the state the rules had
resolved, because the body advanced in the wrong order rather than all at once.

## What a tick serves

Superseded (2026-10-22) by [0021](0021-drain-the-whole-queue.md): a tick serves
every cell the queue holds, so the rail, the `ceil(Q / cap)` sweep and the
queue's halves are gone. This section stays as the record of the rail.

A tick drains the whole queue below `MAX_CELLS_PER_TICK`, which is a rail rather
than the mechanism. A designed scene never reaches it: the ball's queue holds
1,110,768 cells, a queued-cell count, while the ball itself holds 1,097,917
grains, and the rail is 2,000,000. A pathological scene, a `World`
filled with granular cells, would otherwise stall the simulation for minutes in
one tick, and the rail bounds it at about 414 ns a drained cell and 106 ns a
committed move, roughly one second a tick and a rate below 1 Hz. The value is a
count and not a duration, so a tick's outcome never depends on the machine.

Where the rail binds, the Update queue's sweep discipline keeps the scene fair
rather than starving it. The queue holds a ready half and a served half, both in
`(y, x, z)` order, with the sweep's front and whether the sweep is spent. A tick
serves the ready half's lowest cells up to the rail. A mover's destination joins
the served half, so the served mark follows the grain and one grain is not served
twice in a sweep. A wake joins the ready half only while the sweep still has a
turn for that cell, which means the sweep has not spent its ready half and the
front has not passed the cell; every other wake waits in the served half. The
halves swap only at the start of a drain, when the ready half is empty, because a
swap anywhere else would serve a cell twice in a sweep and its grain would move
twice in a tick. A scene above the rail therefore serves `cap` cells a tick. The
first sweep of a freshly seeded queue takes exactly `ceil(Q / cap)` ticks,
because every seeded grain is already in the queue and no wake can add one. A
later sweep can take more ticks, because a wake the front has not passed rejoins
it, and every cell is still served once before any cell is served twice. The
full lattice's 8,259,231 granular cells against the 2,000,000 rail is a
five-tick first sweep, so its grains advance one cell every five ticks rather
than starving.

The mark following the grain is also what makes a wake's wait meaningful. Giving
a wake priority over its turn reproduces the earlier starvation exactly, which is
the property this discipline exists to remove.

## The measurements

The figures below are the sand ball's, 1,097,917 grains with y from -1919 to
-1791, driven through the real commit path in release over 128 ticks.

- Physics: 1,097,917 grains move on every tick through tick 128, the span stays
  at 128, the lowest y is -1919 - t and the highest y is -1791 - t, and the
  bottom reaches the floor at tick 128. The body descends exactly one cell a
  tick.
- Tick cost: 555 to 592 ms a tick for 128 ticks. The drain is 437 to 475 ms,
  `edit_world` is about 117 ms for 2,195,834 edits, and the snapshot fold is
  under 0.3 ms.
- Commit scale: linear at about 53 ns per edit, the same per-edit cost as
  today's 25,706 edits in 1.7 ms, so the commit is proportional and bounded
  rather than superlinear.
- Where the cost is: the drain is 80% of the tick, and the overlay's bookkeeping
  is the largest part of the drain, at about 210 ns per edit. Any later cost work
  belongs in the drain, not in the commit.
- The resulting rate: about 1.7 ticks a second, so the ball's 128-cell fall takes
  roughly 75 seconds of wall time. That is 128 ticks at about 0.58 s a tick. The
  frame rate is unaffected, because the renderer and the host's ordinary frame
  path take no `World` lock.

## Consequences

- The simulation's clock absorbs the shortfall. A tick that costs more than the
  33 ms frame period makes the `World`'s rate fall below the profile's 30 Hz
  rather than making the `World` advance partially. The player and the camera
  stay on the tick clock, so the body and the player share one clock. This amends
  the clock claim in
  [0012](0012-simulation-thread-and-clock-ownership.md).
- The simulation's message loop merges the run of frames queued behind the first
  one into a single update, so a slow tick does not leave the host answering
  input from a minute ago. The elapsed times sum, the newest movement sample and
  the newest jump edge survive, and the first message that is not a frame is
  deferred so it keeps its position. The scheduler's five-tick catch-up cap then
  discards the excess and `UpdateReport::discarded` reports it. This is ticket 04
  of the in-tick drain work.
- The read lock still covers the whole of evaluation, so a host raycast runs
  beside a tick. A host edit waits for the commit window, which the write lock
  owns, and not for evaluation.
- The Falling granular rule is unchanged. Every destination is at `src.y - 1` and
  the drain serves ascending y, so a destination is always a cell the scan has
  already passed. A grain still moves at most one cell or one diagonal per
  evaluation, and a column still shifts one grain at a time. The in-tick
  prototype's moved-this-tick guard never fired once in 200 ticks, which is why
  the rule needs none.
- The `World` mutation cost per edit is unchanged, so
  [0018](0018-cap-the-per-tick-grain-work.md)'s measurements of the commit still
  hold; what 0018 got wrong is the cap's role and the prefix claim, which this
  record corrects.
- Determinism is unchanged. The served order is still the queue's `(y, x, z)`
  order, so both determinism tests hash the same across two runs.

## Status

accepted (2026-10-15). Records the in-tick drain from the in-tick drain spec,
`.scratch/in-tick-drain/spec.md`, and supersedes two statements in
[0018](0018-cap-the-per-tick-grain-work.md): that the processed cells are the
queue's prefix in `(y, x, z)` order and that the cap is the mechanism. Amends
**Simulation tick**, **Update queue** and **Cell cap** in `GLOSSARY.md`, adds
**Sweep**, and replaces the sand ball's timings in `README.md`. Raises the cap
to a 2,000,000-cell rail.
Amended (2026-10-22) by [0021](0021-drain-the-whole-queue.md): the rail is
removed, so a tick drains the whole queue and **Sweep** and **Cell cap** leave
`GLOSSARY.md`.
