# The Update queue is ordered and one tick drains a fixed count

`UpdateQueue` is a deterministic ordered set keyed by `(y, x, z)`, the order the
drain has always used, and one tick pops and processes at most the cell cap,
`MAX_CELLS_PER_TICK`, 4096. The cells a tick does not reach stay queued with
their wakes, so a scene with millions of Falling granular cells settles over
many ticks instead of stalling one. The order and the destination-claim winner
are unchanged for the cells a tick does process: the set iterates by key, and
the processed cells are the queue's prefix in that key.

The cap is the second fixed bound on an update's work, beside the scheduler's
catch-up cap in
[0012](0012-simulation-thread-and-clock-ownership.md). That one bounds a
frame's ticks and this one bounds a tick's voxel work, so neither an owed
backlog nor a large granular scene can stretch one update.

## Why the queue is ordered

The drain resolves a contested destination by processing order, and claims are
made in that order, so the queue's order is part of the simulation's result
rather than an implementation detail. Taking the queue's prefix is what makes
the cap deterministic: one scene and one input drain the same cells in the same
sequence on every run, and the cells behind the cap only wait.

The ordered set is a `BTreeSet` of cells, and a tick splits off everything from
the first cell it does not reach. Splitting is logarithmic in the queue, so the
tick pays for the cells it processes and not for the backlog behind them. The
seed still takes the generator's list rather than scanning the World, the cheap
path from the world-generation issue 10, though the set it builds is ordered
rather than hashed, so seeding is O(n log n) in the granular cells where the
hash set it replaced was O(n). Measured at activation, which is the only place
that cost lands: 13.6 ms for the 512 footprint's 129,627 grains, 67 ms for
1024's 516,579, 265 ms for 2048's 2,065,906, and 1.10 s for the full lattice's
8,259,231. The full-lattice figure is about 4.5 times issue 10's 4.9 s scan,
which it replaced, so the ordered set costs less there than the scan did even
with the extra factor.

## Why 4096

The tick tripwire in [0012](0012-simulation-thread-and-clock-ownership.md) is a
4 ms p95 tick wall time, evaluation plus commit. The parent of this decision
measured the generated surface's scattered sand at about half a microsecond per
grain, so 4096 cells is about 2 ms against the budget, and the dense block the
tripwire sweep uses costs far less per cell. The figure is a count rather than a
duration because a duration would make the tick's outcome depend on the
machine.

At the full lattice the measured rule work is 1.7 ms, so the count lands where
the estimate put it. It is not lowered to bring the whole tick under the budget,
because the rule work is not what carries that tick over. See the consequences.

## Considered options

- **A hash set re-sorted each tick and truncated to the cap**. Rejected: the
  sort covers the whole queue every tick, so a tick stays O(n log n) in the
  grains that have not settled, which is the cost the cap exists to bound.
- **A `BTreeSet` of regions or tiles rather than of cells**. Not needed: the
  cap bounds a tick to a few thousand cells, so the split's own logarithmic
  cost is paid once per tick and not per grain.
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
  snapshot fold was fixed.
- The lowest cells drain first and a moved cell is queued at its destination,
  one level lower. A scene that keeps falling, such as the tripwire's spaced
  sand layers, therefore drains the same lowest cells for as long as they have
  room to fall, and the cells behind them wait. A scene that settles, such as
  the generated surface, drains in ascending bands.
- The tripwire sweep no longer crosses its budget on the active-cell axis,
  because the cap bounds a tick's rule work whatever the scene holds. Its
  `p95 chunks` column is the Micro-chunks the tick actually touched: at 1,000
  active cells, below the cap, the tick drains the whole scene and the column is
  that scene's own footprint, 16 chunks; at 1,000,000 active cells it is 168.
  The column tracks the active count below the cap and stops tracking it above.
  Before the cap the rule work crossed 4 ms at 100,000 active cells at two of
  the three occupancies, and the whole tick took 250 ms at 1,000,000.
- Both benches' budget check holds the rule work, and a tick over budget with
  its rule work inside it is reported as `COMMIT` rather than mixed into it.
  Moving the check is a deliberate narrowing, and it leaves the tripwire in
  [0012](0012-simulation-thread-and-clock-ownership.md) partly unmet: that
  record's threshold is the whole tick, and the whole tick still crosses 4 ms at
  the bistro occupancy and at the larger generated footprints. What this
  decision establishes is that the rule work, which the cap owns, is inside the
  budget. The commit window that crosses it is not.
- That window carries `edit_world`'s compile of every touched Micro-chunk, which
  scales with the cells the tick drains and so with the cap, and its clone of
  the renderer's tracked set, which scales with the loaded world. Which term
  dominates the full-lattice surface's 16.1 ms commit is unmeasured: the bench
  does not separate them, and lowering the cap would shrink the compile term but
  not the clone. The clone alone is about 3.2 ms at bistro's 1,748,065 tracked
  Micro-chunks, which is most of that occupancy's commit. Separating the two,
  and deciding whether the renderer's tracked set should be cloned per tick at
  all, is open work.
- `generated_surface_tick_timings` in `app/sim_host/bench.rs` drives a
  Generation's own Sand surface, the scene the cap exists for. Its p95 over 256
  ticks is 3.0 ms at the 512 footprint, 4.0 ms at 1024, 7.4 ms at 2048, and
  17.6 ms at the full lattice, of which the rule work is 1.0, 1.2, 1.6, and
  1.8 ms. The rule work is what the cap bounds, it stays inside the 4 ms budget
  at every footprint, and it is what the bench asserts. The full-lattice tick is
  over the frame budget and stays over it, which is the gap recorded above. The
  surface settles over thousands of ticks rather than stalling one, which is the
  criterion this decision took on.
- A full-lattice surface's first ticks reach the deepest level, where no cell
  can move, so the drain settles them one tick at a time and the first moves
  wait for the queue to reach a level with a lower neighbour. The surface bench
  drives 256 ticks for that reason.

## Status

accepted (2026-10-05). Records the Update queue's order and the cell cap from
the world-generation issue 12. Adds **Update queue** and **Cell cap** to
`GLOSSARY.md` and widens **Simulation tick** with the cap. The unit tests in
`sim/physics/rules.rs` pin the cap, the unchanged processed prefix, and the
repeated sequence, and `tests/falling_granular.rs` pins the cap through the
sim's tick boundary.
