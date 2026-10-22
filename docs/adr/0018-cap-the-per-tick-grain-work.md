# The Update queue is ordered and its cell cap is a rail

`UpdateQueue` is a deterministic ordered set keyed by `(y, x, z)`, the order the
drain has always used, and one tick drains the whole queue below the cell cap,
`MAX_CELLS_PER_TICK`. The set iterates by key, so a contested destination is
still resolved by the order the tick serves in. The cells a tick serves are not
the queue's prefix in that key, which is what this record first said: once a
sweep is under way the queue holds a served half and an unserved half, so a tick
serves the lowest cells of the unserved half in key order, and "the queue's
prefix" no longer names them. That prefix claim is one of the two statements
[0020](0020-in-tick-drain.md) supersedes.

This record was written when the cap was the mechanism and stood at 4096, and
[0020](0020-in-tick-drain.md) supersedes two of its statements: that the
processed cells are the queue's prefix in `(y, x, z)` order, and that the cap is
the mechanism. Read the rest of this record as the queue's order, which 0020
keeps, plus the history of why the cap was introduced. Each superseded statement
is marked where it appears. [0021](0021-drain-the-whole-queue.md) then removes
the cap, so what outlives this record is the queue's order alone: the cap, its
2,000,000 value and the sweep discipline are history.

The cap is the second fixed bound on an update's work, beside the scheduler's
catch-up cap in
[0012](0012-simulation-thread-and-clock-ownership.md). That one bounds a
frame's ticks. The cap's job here has narrowed to bounding a pathological
granular scene, a `World` filled with granular cells, which would otherwise
stall the simulation for minutes in one tick.

## Why the queue is ordered

The drain resolves a contested destination by processing order, so the queue's
order is part of the simulation's result rather than an implementation detail.
That claim outlives the cap, and [0020](0020-in-tick-drain.md) relies on it: the
order decides a contested destination whether or not a cap cuts the drain short.
The cap's own part in the claim has changed. It used to be what made the drain
deterministic, since taking the queue's prefix fixed which cells a tick reached.
The drain is deterministic without it now, because below the rail a tick serves
every cell, and above the rail the queue's sweep discipline fixes which cells the
tick reaches.

The ordered set is a `BTreeSet` of cells, and a tick still splits off everything
from the first cell it does not reach, in the one pass that also fixes the
boundary. Splitting is logarithmic in the queue, so the tick pays for the cells
it serves and not for the backlog behind them, whether the backlog is one tick's
worth or a sweep's. The seed still takes the generator's list rather than
scanning the World, the cheap path from the world-generation issue 10, though
the set it builds is ordered
rather than hashed, so seeding is O(n log n) in the granular cells where the
hash set it replaced was O(n). Measured at activation, which is the only place
that cost lands: 13.6 ms for the 512 extent's 129,627 grains, 67 ms for
1024's 516,579, 265 ms for 2048's 2,065,906, and 1.10 s for the full lattice's
8,259,231. The full-lattice figure is about 4.5 times issue 10's 4.9 s scan,
which it replaced, so the ordered set costs less there than the scan did even
with the extra factor.

## Why the cap is 2,000,000

4096 was chosen as the mechanism's value: the tick tripwire in
[0012](0012-simulation-thread-and-clock-ownership.md) is a 4 ms p95 tick wall
time, evaluation plus commit, the generated surface's scattered sand measured at
about half a microsecond per grain, and 4096 cells was about 2 ms against the
budget. [0020](0020-in-tick-drain.md) raised it to 2,000,000 and made it a rail,
because a tick now evaluates the whole queue and the value only has to bound a
pathological scene. The measured drain is the rail's basis: about 414 ns a
drained cell and 106 ns a committed move, so 2,000,000 cells is roughly a second
a tick and a rate below 1 Hz. The figure is a count rather than a duration
because a duration would make the tick's outcome depend on the machine, which is
the reason 4096 was a count too.

At the full lattice the measured rule work under the 4096 cap was 1.7 ms, which
is where the estimate put it, and 4096 was not lowered to bring the whole tick
under the budget, because the rule work was not what carried that tick over. That
reasoning no longer applies to the value the code carries: 2,000,000 is the
measured rail. See the consequences.

## Considered options

- **A hash set re-sorted each tick and truncated to the cap**. Rejected: the
  sort covers the whole queue every tick, so a tick stays O(n log n) in the
  grains that have not settled, which is the cost the cap exists to bound.
- **A `BTreeSet` of regions or tiles rather than of cells**. Not needed: the
  split's own logarithmic cost is paid once per tick and not per grain, whether
  the tick serves a few thousand cells or the whole queue.
- **An arrival-ordered queue**. Rejected: the order is the tie-break between
  two grains reaching for one destination, and every falling-granular
  walkthrough is pinned against `(y, x, z)`. An arrival order changes results
  the tests hold fixed and buys nothing the ordered set does not have.
- **A wall-clock budget per tick**. Rejected: it makes the drain depend on the
  machine and the run, and replaying a scene would no longer reproduce it.
- **Dropping the cells past the cap until the next activation**. Rejected: they
  would never settle, so a granular surface would stay half moved instead of
  finishing over ticks.

## Consequences

- A queue holds every cell that has not settled, so the backlog is bounded by
  the scene rather than by a tick, and the queue's own memory is proportional to
  the granular cells of the World.
- A Falling granular cell blocks the player only while it is not queued, so a
  cell waiting behind the cap neither blocks nor has settled, and the player can
  fall through a large surface until the queue reaches it. Before the cap that
  window was one tick. The alternative was an uncapped tick, which the parent
  issue measured at about five seconds for the first full-lattice tick after its
  snapshot fold was fixed. The wait is now bounded by a sweep: a cell behind the
  rail is served within `ceil(Q / cap)` ticks on the first sweep of a freshly
  seeded queue, which the full lattice's 8,259,231 granular cells make five, so
  no cell waits indefinitely. A later sweep can take more ticks, because a wake
  the front has not passed rejoins it, but every cell is still served once
  before any cell is served twice.
- The lowest cells drain first and a moved cell is queued at its destination,
  one level lower. A scene that keeps falling, such as the tripwire's spaced
  sand layers, therefore drains the same lowest cells for as long as they have
  room to fall, and the cells behind them wait. A scene that settles, such as
  the generated surface, drains in ascending bands. A scene above the rail no
  longer drains this way: the queue's sweep discipline
  ([0020](0020-in-tick-drain.md)) keeps a moved cell in the served half for the
  rest of its sweep, so the lowest cells lose their turn once served and every
  cell is served once before any cell is served twice. Below the rail a tick
  serves the whole queue, so the case does not arise.
- The tripwire sweep no longer crosses its budget on the active-cell axis,
  because the cap bounded a tick's rule work whatever the scene held. Its
  `p95 chunks` column is the Micro-chunks the tick actually touched: at 1,000
  active cells, below the cap, the tick drains the whole scene and the column is
  that scene's own footprint, 16 chunks; at 1,000,000 active cells it is 168.
  The column tracks the active count below the cap and stops tracking it above.
  Those figures were taken with the 4096 cap, so they record what the cap did
  rather than what the 2,000,000 rail does. Before the cap the rule work crossed
  4 ms at 100,000 active cells at two of the three occupancies, and the whole
  tick took 250 ms at 1,000,000.
- The in-tick drain reverses that axis. A tick now drains the whole queue, so
  the rule work follows the scene and the sweep crosses 4 ms on the active-cell
  axis again. On the 2026-10-15 run the rule work is 293.800µs, 2.428ms,
  28.091ms and 435.298ms at 1,000, 10,000, 100,000 and 1,000,000 active cells at
  the synth occupancy, 461.000µs, 8.331ms, 24.784ms and 434.709ms at church,
  and 386.000µs, 1.628ms, 26.653ms and 433.336ms at bistro. The whole tick is
  481.400µs, 4.039ms, 41.207ms and 579.831ms at synth, 3.546ms, 19.580ms,
  38.438ms and 578.930ms at church, and 10.128ms, 10.151ms, 48.827ms and
  596.400ms at bistro. The `p95 chunks` column is 16, 108, 468 and 5,096 at
  those points against the capped run's 16 and 168: it follows the scene all the
  way to 1,000,000 active cells now, because a tick serves the whole queue at
  every sweep point below the rail.
- Both benches' budget check held the rule work, and a tick over budget with
  its rule work inside it is reported as `COMMIT` rather than mixed into it.
  Moving the check was a deliberate narrowing, and it leaves the tripwire in
  [0012](0012-simulation-thread-and-clock-ownership.md) partly unmet: that
  record's threshold is the whole tick, and the whole tick still crosses 4 ms at
  the bistro occupancy and at the larger generated extents. What this
  decision established is that the rule work was inside the budget. The commit
  window that crosses it is not. [0020](0020-in-tick-drain.md) left the check as
  it stood, and the sand ball's own tick is a tick the check was never sized
  for: a full ball tick is 555 to 592 ms, about 140 times the 4 ms threshold.
- The in-tick drain then restates the check against the rail. Both benches'
  check now holds the rail's bound, `MAX_CELLS_PER_TICK` cells of rule work at
  the measured 414 ns a cell, which is 828 ms, and the 4 ms threshold is the
  whole-tick tripwire reported beside it. A point whose rule work crosses the
  tripwire but stays inside the rail is `DRAIN`, a large scene draining as
  designed. A point whose rule work crosses the rail's bound is `OVERRUN`, which
  is the failure the check asserts. No sweep point crosses the rail up to
  1,000,000 active cells, so the sweep passes at every point instead of failing
  by design.
- That window carries `edit_world`'s compile of every touched Micro-chunk, which
  scales with the cells the tick drains, and its clone of the renderer's tracked
  set, which scales with the loaded world. That second term is now the one the
  rail cannot shrink: at a full-lattice surface the tick drains 8,259,231 cells
  over the first sweep's five ticks, so the commit's compile term is spread
  across five ticks while the tracked-set clone is paid on every tick that
  commits. Which term dominates the full-lattice surface's 16.1 ms commit is
  still unmeasured: the bench does not separate them. The clone alone is about
  3.2 ms at bistro's 1,748,065 tracked Micro-chunks, which is most of that
  occupancy's commit. Separating the two, and deciding whether the renderer's
  tracked set should be cloned per tick at all, is open work.
- `generated_surface_tick_timings` in `app/sim_host/bench.rs` drives a
  Generation's own Sand surface, the scene the cap was written for. Its p95 over
  256 ticks is 3.0 ms at the 512 extent, 4.0 ms at 1024, 7.4 ms at 2048, and
  17.6 ms at the full lattice, of which the rule work is 1.0, 1.2, 1.6, and
  1.8 ms. The rule work stays inside the 4 ms budget at every extent, and it is
  what the bench asserts. The full-lattice tick is over the frame budget and
  stays over it, which is the gap recorded above. The surface settles over
  thousands of ticks rather than stalling one, which is the criterion this
  decision took on. At the full lattice the surface's queue is the five-tick
  first sweep [0020](0020-in-tick-drain.md) describes, so the 8,259,231 granular
  cells are served across five ticks rather than in one 2,000,000-cell tick.
- The same surface on the in-tick drain, 2026-10-15: the p95 tick is 531.600µs
  at the 512 extent, 1.618ms at 1024, 3.840ms at 2048 and 9.300µs at the full
  lattice, of which the rule work is 180.900µs, 256.600µs, 772.800µs and
  9.300µs. The 512, 1024 and 2048 extents hold 48,911, 202,221 and 1,800,792
  grains, below the 2,000,000-cell rail, so a tick drains their whole queue and
  the rule work is the scene's own size. The full lattice holds 8,432,216
  grains, above the rail, so its first sweep is five ticks. That count is this
  run's, where the record's own figure for the same surface is 8,259,231; both
  stand as measured, and the difference between them is not established here.
  Its p95 is 9.300µs, which is an idle tick rather than that tick's cost:
  nothing moves, so the queue can only shrink, and the ticks that drain the
  rail's 2,000,000 cells fall below the 95th percentile of 256. The full lattice
  also moves nothing in 256 ticks and fails the bench's churn assertion. The
  same point failed at `main` with the 4096 cap, with the same zero moves, so
  the in-tick drain neither introduced nor fixed that failure.
- A full-lattice surface's first ticks reach the deepest level, where no cell
  can move, so the drain settles them one tick at a time and the first moves
  wait for the queue to reach a level with a lower neighbour. The surface bench
  drives 256 ticks for that reason.

## Status

accepted (2026-10-05). Records the Update queue's order and the cell cap from
the world-generation issue 12. Adds **Update queue** and **Cell cap** to
`GLOSSARY.md` and widens **Simulation tick** with the cap. The unit tests in
`sim/physics/rules.rs` pinned the cap, the unchanged processed prefix, and the
repeated sequence, and `tests/falling_granular.rs` pinned the cap through the
sim's tick boundary. Amended (2026-10-15) by
[0020](0020-in-tick-drain.md): a tick now drains the whole queue and the cap is
a 2,000,000-cell rail rather than the mechanism, so the cells a tick serves are
the unserved half's lowest cells rather than the queue's prefix, and the cap no
longer makes the drain deterministic.
The amended statements are marked as they appear, and the record is kept rather
than rewritten because the queue's order, which is what outlives the cap, is
this record's decision. `the_processed_cells_match_the_uncapped_drain` still
passes, because the served prefix resolves the same way whether or not a rail
cuts the drain short. `a_tick_stops_at_the_cap_and_leaves_the_rest_queued` lost
its subject, since a 4097-grain row does not reach a 2,000,000-cell rail, and
`a_row_under_the_rail_is_served_in_full_each_tick` asserts the inverse property
through the same boundary.

Amended (2026-10-22) by [0021](0021-drain-the-whole-queue.md): the cap is
removed and a tick serves every queued cell, so this record is the history of
the cap and the queue's order. `a_row_under_the_rail_is_served_in_full_each_tick`
is now `a_row_across_the_lattice_is_served_in_full_each_tick`, and the capped
tests in `sim/physics/rules.rs` are gone with the cap.
