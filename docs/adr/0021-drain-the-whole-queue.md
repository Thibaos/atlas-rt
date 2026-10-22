# A tick drains the whole update queue

`drain` serves every cell the update queue holds. The cell cap
`MAX_CELLS_PER_TICK` is gone with the boundary that cut the drain, and the
queue keeps two sets: the cells this tick serves and the cells its moves and
wakes queue for the next. A grain still moves at most one cell a tick, because
a destination is one cell below its source and joins the next tick's set.

## Why

A tick is one step of the World, and the commit is where that step becomes
visible. A cap serves part of the scene per tick, so a sweep spans
`ceil(Q / cap)` ticks and every commit in between shows a World that some cells
have left and others have not. A body that straddles the cut is served in two
different ticks: the cells below the cut move while the cells above it wait,
and the queue's cells do not block the player until their turn, which
[0018](0018-cap-the-per-tick-grain-work.md) records as its own consequence.
The sweep discipline [0020](0020-in-tick-drain.md) added keeps that fair. It
does not make it coherent: every cell is served once per sweep, and a sweep is
not a commit.

Cutting the work does not make it smaller. A sweep costs one evaluation per
queued cell whether it takes one tick or `ceil(Q / cap)`, so the cap decides
when the cost is paid and not what it is. Measured on the generated surface's
full lattice, 8,432,216 grains against the 2,000,000 rail: five ticks and
1.320 s with the rail, one tick and 1.321 s without, and no moves either way
(`generation_first_tick_timings`, release, 2026-10-22).

## Consequences

- The World advances as a whole or not at all. A tick that costs more than a
  frame period makes the rate fall, which is the clock
  [0012](0012-simulation-thread-and-clock-ownership.md) already owns, and
  nothing advances partially to keep up.
- A tick's cost follows the scene, so the simulation thread is busy for the
  whole step. A command, a pause or an activation submitted during a long tick
  waits for it. The host keeps presenting the last committed World meanwhile.
- A full-lattice generation costs one activation tick of about 1.3 s, where the
  rail spent the same total across five ticks. The rate is below 1 Hz for that
  tick, as it was for each of the rail's.
- The queue keeps no front and no spent mark, because no cell is left unserved
  for a later tick. A wake always joins the next tick's set, so the halves mean
  this tick's cells and the next tick's rather than served and unserved within a
  sweep.
- The budget check in `app/sim_host/bench.rs` no longer holds a fixed bound. It
  holds the rule work per drained cell, 600 ns against the 310 ns the sweep
  measures at 1,000,000 active cells, so the check follows the scene and a
  per-cell regression of about twice the cost is what fails it.

## Considered options

- **A smaller cap that keeps the 4 ms frame budget.** Rejected: a designed
  scene above it would fall one cell per `ceil(Q / cap)` ticks, which is the
  grain-phase failure 0020 removed by raising the cap to a rail.
- **A wall-clock budget per tick.** Rejected: the tick's outcome would depend
  on the machine and the run, which 0018 rejected for the same reason.
- **Dropping the cells a tick does not reach.** Rejected: they would never
  settle, 0018's own reason against it.

## Status

accepted (2026-10-22). Removes the cap from
[0018](0018-cap-the-per-tick-grain-work.md) and the rail and sweep discipline
from [0020](0020-in-tick-drain.md), drops **Cell cap** and **Sweep** from
`GLOSSARY.md`, restates **Simulation tick** and **Update queue**, and replaces
the fixed rule-work bound in [0012](0012-simulation-thread-and-clock-ownership.md)'s
tripwire check with a per-drained-cell bound.
