# The Update queue is ordered and its cell cap is a rail

`UpdateQueue` is a deterministic ordered set keyed by `(y, x, z)`, the order the
drain has always used, and one tick drains the whole queue below the cell cap,
`MAX_CELLS_PER_TICK`. The set iterates by key, so the cells a tick serves are the
queue's prefix in that key, and a contested destination is still resolved by the
order the tick serves in.

This record was written when the cap was the mechanism and stood at 4096, and
[0020](0020-in-tick-drain.md) supersedes two of its statements: that the cap
bounds a tick's work, and that the cap is what makes the drain deterministic.
Read the rest of this record as the queue's order, which 0020 keeps, plus the
history of why the cap was introduced. Each superseded statement is marked where
it appears.

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
  rail is served within `ceil(Q / cap)` ticks, which the full lattice's
  8,259,231 granular cells make five, so no cell waits indefinitely.
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
- Both benches' budget check holds the rule work, and a tick over budget with
  its rule work inside it is reported as `COMMIT` rather than mixed into it.
  Moving the check is a deliberate narrowing, and it leaves the tripwire in
  [0012](0012-simulation-thread-and-clock-ownership.md) partly unmet: that
  record's threshold is the whole tick, and the whole tick still crosses 4 ms at
  the bistro occupancy and at the larger generated extents. What this
  decision establishes is that the rule work is inside the budget. The commit
  window that crosses it is not. The check itself is unchanged by
  [0020](0020-in-tick-drain.md), and the sand ball's own tick is a tick the
  check was never sized for: a full ball tick is 555 to 592 ms, about 140 times
  the 4 ms threshold.
- That window carries `edit_world`'s compile of every touched Micro-chunk, which
  scales with the cells the tick drains, and its clone of the renderer's tracked
  set, which scales with the loaded world. That second term is now the one the
  rail cannot shrink: at a full-lattice surface the tick drains 8,259,231 cells
  over a five-tick sweep, so the commit's compile term is spread across five
  ticks while the tracked-set clone is paid on every tick that commits. Which
  term dominates the full-lattice surface's 16.1 ms commit is still unmeasured:
  the bench does not separate them. The clone alone is about 3.2 ms at bistro's
  1,748,065 tracked Micro-chunks, which is most of that occupancy's commit.
  Separating the two, and deciding whether the renderer's tracked set should be
  cloned per tick at all, is open work.
- `generated_surface_tick_timings` in `app/sim_host/bench.rs` drives a
  Generation's own Sand surface, the scene the cap was written for. Its p95 over
  256 ticks is 3.0 ms at the 512 extent, 4.0 ms at 1024, 7.4 ms at 2048, and
  17.6 ms at the full lattice, of which the rule work is 1.0, 1.2, 1.6, and
  1.8 ms. The rule work stays inside the 4 ms budget at every extent, and it is
  what the bench asserts. The full-lattice tick is over the frame budget and
  stays over it, which is the gap recorded above. The surface settles over
  thousands of ticks rather than stalling one, which is the criterion this
  decision took on. At the full lattice the surface's queue is the five-tick
  sweep [0020](0020-in-tick-drain.md) describes, so the 8,259,231 granular cells
  are served across five ticks rather than in one 2,000,000-cell tick.
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
a 2,000,000-cell rail rather than the mechanism, so the prefix claim holds only
of the cells a tick serves and the cap no longer makes the drain deterministic.
The amended statements are marked as they appear, and the record is kept rather
than rewritten because the queue's order, which is what outlives the cap, is
this record's decision. `the_processed_cells_match_the_uncapped_drain` still
passes, because the served prefix resolves the same way whether or not a rail
cuts the drain short. `a_tick_stops_at_the_cap_and_leaves_the_rest_queued` lost
its subject, since a 4097-grain row does not reach a 2,000,000-cell rail, and
`a_row_under_the_rail_is_served_in_full_each_tick` asserts the inverse property
through the same boundary.
