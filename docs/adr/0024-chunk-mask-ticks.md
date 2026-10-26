# A tick serves a Micro-chunk at a time

`UpdateQueue` holds one 512-bit mask per queued Micro-chunk instead of a set of
cell coordinates. A cell's bit is at byte `y * 8 + x` and bit `z`, so a byte
holds the z bits of one (y, x). That is the transpose of the store's mask,
whose byte `y + 8z` holds the x bits of one (y, z). The transpose orders the
bytes by y then x and the bits by z, so a chunk's 64 bytes yield its cells in
ascending y, then x, then z. Queued chunks are walked in chunk order and each
chunk's cells in that local order, so the drain serves the same `(y, x, z)`
sequence the coordinate set served, and a contested destination resolves the
same way. [0018](0018-cap-the-per-tick-grain-work.md) makes that order part of
the result.

The overlay holds each touched chunk's state after this tick's moves so far, and
it is the post-tick truth for that chunk. It is one mask in the store's layout,
an optional uniform material for a chunk whose grains all share one, a 512-byte
per-cell material array only for a chunk that mixes materials, and a count of
the moves the chunk served. `material` answers from the World when the cell is
unchanged, from the uniform marker, or from the array. `supports` and
`in_flight` need no extra structure: a cell set in the overlay and absent from
the World moved in this tick, so it does not support a grain above it.

A tick may translate a maximal run of chunks as one mask write per chunk. A
chunk translates when every grain in it falls straight down: for rows 1 to 7 the
cell below falls inside the chunk, and for row 0 the cell below is open or held
by a grain that moves. Two conditions narrow that rule. A chunk must hold more
cells than `REWRITE_THRESHOLD`: at or under it, writing the chunk whole costs
more than its edits, so it stays on the per-cell path. The queue must hold every
cell of it, because the translation moves the whole chunk while a tick serves
only the queue's cells, so a grain the tick does not serve would move anyway.
Runs are decided per chunk per tick, bottom-up from the pre-tick masks, and
never cached.

The boundary rule keeps the translation exact for a body whose bottom row is
occupied. A translating chunk's row 0 falls into the chunk below and its row 7
is filled from the chunk above. When the neighbour is in the run, the
neighbour's translation owns the transfer. When the chunk below is outside the
run, the run pass moves the chunk's row 0 into it first, with `apply_move` and a
destination-only edit, because the chunk's own translation already vacates the
row. When the chunk above is outside the run, the translating chunk leaves row 7
empty and the above chunk's row-0 grains move into it through the per-cell pass.
Without that rule the same grain is represented twice, or a source cell is
cleared with no destination.

The commit materializes the overlay. A touched chunk whose move count exceeds
`REWRITE_THRESHOLD`, 4, is written whole through `write_entry` from the
overlay's mask and materials, and every per-cell edit whose position lies in a
rewritten chunk is dropped, because the entry already holds that move. The
remaining edits go through `apply_edits`, and both snapshot lists fold into one
batch through `SnapshotFold`, so the tracked set is planned once. `edit_world`
is the whole of the commit only when no chunk crosses the threshold, and the
overlay reports the most moves it served, so that answer costs one comparison.

## Why

A tick costs about 420 ns per grain around a rule decision worth about 10 ns. On
the sand ball's 1,097,917 grains the tick is 555 to 592 ms: 200 ms for the
queue's 1.1 million inserts and 3.3 million wake lookups, 50 ms for the
overlay's eight hash operations per grain, 42 ms for four World reads per grain,
and 108 ms for 2.2 million rank and memmove edits in the store. The commit
replays every move as an edit at about 106 ns a move.

A probe translated the same ball with chunk masks and matched the per-cell path
cell for cell. It cost 1.44 ms for the whole chunk pass over 5,780 chunk slots,
mask reads, shifted masks and `write_entry` included, 0.27 ms of that for the
shifts alone, and 2.3 ms to compile the 2,680 snapshots that hold content,
against 107.7 ms for the per-cell mutation alone. The ball's free fall is one
rigid translation, so the per-cell path repeats the same decision 1.1 million
times.

A dense chunk is cheaper to write whole. A whole-entry write measured about
250 ns for a ball chunk against about 106 ns a move as an edit, so the crossover
sits near four moves: four moves cost about 424 ns as edits against about 250 ns
as an entry. The commit compares strictly, with `moves > threshold`, so a
four-move chunk keeps its edits and five moves and up are written whole.

Stage A changes the representation and emits the same edits, so the existing
suite proves it and a differential test proves it again against the unmodified
per-cell path. Stage B adds the run translation, and the per-cell pass moves
whatever crosses a run boundary.

The rain has no such redundancy. A grain there is alone in its Micro-chunk, so
it costs a chunk's overlay seed and a chunk's queue slot for one cell, and
neither stage reduces the number of chunks a move touches. Its plateau measured
110 ms p95 before these changes and 143 to 160 ms after, which is what the chunk
granularity costs a scene whose chunks hold one grain. That is why a run is
gated per chunk per tick rather than switched on for a scene.

## Consequences

- The serve order is unchanged, so a contested destination resolves as it did
  and the order stays part of the result.
- The tests in `sim/physics/rules.rs` that compare a `Vec<VoxelEdit>`
  exactly stay written against the per-cell policy, because stage A emits the
  same edits.
- The per-cell path remains the oracle and the fallback. `DrainPolicy` selects
  `PerCell` or `Runs`, is threaded like `ParityPolicy`, and is set by the
  differential test and the benches. No standalone flag exposes it.
- A tick still serves every queued cell. A translated cell counts as served, so
  [0021](0021-drain-the-whole-queue.md)'s whole-queue contract holds and its
  600 ns per-drained-cell check stays meaningful.
- A forced cell budget still refuses a batch that would exceed it. The
  projection counts a rewritten chunk's delta from its overlay mask and its
  store entry.
- The rewrite filter costs about 10 ns per edit and is skipped when no chunk is
  rewritten.
- The rain's 262,000 chunk snapshots a tick are untouched, and its plateau paid
  the chunk granularity on one-grain chunks: 110 ms p95 before, 143 to 160 ms
  after. It needs its own spec.
- The renderer is unchanged. The frame path costs about 6,800 packed chunks a
  tick and is measured at the new tick rate rather than reworked.

## Considered options

- **A rigid body with a transform for the ball.** Rejected: the Micro-chunk is
  the only aggregation unit, because it is what the store, the snapshots, the
  DDA and the region instances already speak.
- **Writing every touched chunk whole.** Rejected: a chunk that moves one cell
  costs about 250 ns as an entry and about 106 ns as an edit, so the rain's
  one-grain chunks and a scattered world would pay more.
- **Caching a chunk's run decision across ticks.** Rejected: the decision reads
  the pre-tick masks, which every tick's moves change.
- **A parallel drain.** Rejected: a parallel pass would have to keep the queue's
  order, which decides a contested destination, and the cost this record removes
  is per-cell bookkeeping rather than a serial step.
- **A renderer change to pack the new batch.** Rejected: the frame path is
  measured, not reworked.

## Status

accepted (2026-10-26). Records the chunk-mask queue, the post-tick chunk
overlay, the run translation and the whole-chunk commit from
`.scratch/chunk-granular/spec.md`. Leaves
[0018](0018-cap-the-per-tick-grain-work.md)'s queue order,
[0020](0020-in-tick-drain.md)'s in-tick state and
[0021](0021-drain-the-whole-queue.md)'s whole-queue drain in force: the drain
still resolves against an immutable World, writes each move into an overlay as
it makes it, and serves every queued cell. Two of 0020's statements are
superseded: the overlay's shape, which was per cell, and its claim that the edit
list and the commit path are unchanged, which no longer holds, because a
translated chunk emits no per-cell edit for the cells it moves and the commit
writes a rewritten chunk whole rather than replaying its moves as edits.
Restates **Simulation tick** and **Update queue** in `GLOSSARY.md` and replaces
the sand ball's timings in `README.md`.
