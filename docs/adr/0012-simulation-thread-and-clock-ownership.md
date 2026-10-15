# The simulation owns its clock and runs off the host thread

The simulation runs on its own thread. Every frame the host sends elapsed time
and buffered input over an unbounded `std::sync::mpsc`, and the simulation
accumulates and decides whether a Simulation tick is due. The host clock is the
only clock. The simulation freezes when frames stop. The World's rate is the
host clock's rate only while a tick fits in a frame period; a tick that costs
more makes the rate fall below the profile's, which
[0020](0020-in-tick-drain.md) records. The simulation still advances only while
frames arrive, and it never advances the World partially to keep up.

The `World` sits behind `Arc<RwLock<World>>`. The application never holds
`&mut World`. Every edit, block breaking, placement, and brush included, is a
command message the simulation applies at commit. Reads are direct: a raycast
and the counts take the read lock against the real `World`, with no mirror and
no request channel.

The tick rate is fixed and comes from the [Player profile](0014-player-contact-and-support-rules.md).
An update runs at most five ticks. If more time is owed, every remaining
whole-tick backlog is discarded, only the sub-tick remainder is kept, and the
drop count is reported in `UpdateReport`. A new, reset, or resumed scheduler
starts with no accumulated time and runs no immediate tick. The frames already
queued behind the one an update starts from are merged into that update, so the
elapsed time one slow tick accumulates arrives as a single owed batch and the
discard reported for it is what the World's rate fell short by, as
[0020](0020-in-tick-drain.md) records.

## Considered options

- **Driving the simulation from the frame loop as `sim.update(&mut world, elapsed, sample)`**. This was the first boundary answer. The thread decision superseded it: the tick moved off the frame loop so that the host clock would be the only clock and the simulation would freeze when frames stop. The one-call-per-frame API survived the move. The `&mut World` argument did not.
- **A mirrored copy of the `World` for host reads**. Rejected: reads take the read lock against the real `World`, so no mirror can drift and no request channel is needed. A raycast is then stale by at most one tick, which is cheaper than keeping a mirror correct. On the sand ball that bound is 555 to 592 ms, so a raycast can report a `World` up to about 0.6 s old. [0020](0020-in-tick-drain.md) has the figures the bound comes from.
- **Diffing host edits in the host before submitting a batch**. Rejected: two writes inside one tick window would diff the second against a `World` that does not have the first yet, and `submit_batch` would derive no-ops that are not true. The simulation diffs at commit instead, where the writes are in order, for 512 hashmap lookups inside the commit window.
- **A second evaluation path chosen now**, a dense local window or batched neighbourhood reads. Rejected as premature. Two evaluation paths maintained before any measurement justifies the second is work with no stated payoff. The tripwire below is the measurement that would justify one.

## Locking discipline

The simulation holds the read lock through evaluation, drops it, takes the
write lock only for the commit and its snapshot emission, and never holds a
lock across a channel send. The host takes the read lock only inside a query,
never across `submit_batch` or a wait.

Because the read lock is shared, the evaluation window does not exclude a host
read: the drain and a raycast run at the same time, which
[0020](0020-in-tick-drain.md) keeps by holding the `World` immutable while the
drain applies each move to its own overlay. The commit is the window that does
exclude a host read. A command submitted during a long drain waits for that drain
to end before its commit starts, so what a dig waits through is the tail of the
evaluation window plus the commit window, and not the whole tick. On the sand
ball the drain is 437 to 475 ms of a 555 to 592 ms tick and the commit is about
117 ms, so the commit window is a fifth of what a dig can wait on. The commit is
still the only window that holds the write lock, and its duration is a metric
rather than a hope.

Activation is the only reset. The new `World` rides inside the activation
message and the simulation swaps it under the write lock itself, then clears
and reseeds the update queue, repositions the player, resets the accumulator
and the buffered jump edge, drops the lock, and replies readiness. No host-side
swap exists, so no tick runs against the new `World` with the old queue and the
old feet. A `clear` is an activation carrying an empty `World`, so it does tick,
with an empty queue. Before the first activation and after `no_world` the
simulation receives input and runs no ticks.

Pause freezes time, not the `World`. It stops accumulation, ignores new jump
edges, and clears the sub-tick remainder and the pending edge. Command edits
buffer while paused and apply in order at the first tick after resume, which
keeps the single commit site. Activation while paused stays paused.

`wait_until_idle` leaves the frame path and survives at load and shutdown only,
where the residency store waits before it reads the packed regions.

The readiness reply is waited on exactly once, at the first frame after
activation, and the wait is bounded by a deadline. The latch that makes it
single is deliberate: a failed wait degrades to draining rather than wedging the
frame loop, and the simulation sends the activation batch ahead of the reply so
a host that is already draining forwards the world before it is told the world
is ready.

## Timing handed to the host

The tick-end push carries the sub-tick remainder as of that tick, plus a snap
flag. The host computes `(remainder + elapsed_since_applying_that_push) /
tick_period`, clamped to `[0, 1]`. The simulation reports and the host
computes. Host-side arrival timestamping was rejected because it folds channel
and frame-scheduling jitter into the value interpolation exists to smooth, and
a reply-channel query buys no accuracy over the remainder that already rides
the push.

The remainder and the snap flag are siblings of `PlayerState` on the push, not
fields of it. Scheduler timing and delivery annotation are not player state.

## Tripwire

The performance trigger is p95 tick wall time, evaluation plus commit, crossing
4 ms. The budget was set against a 60 fps frame target running a 30 Hz tick.
Commit duration reports as a break-out sub-metric inside it, since it is the
only window a frame can stall in. Active-cell throughput is not the tripwire,
because it misses a shard map that got slower at a constant cell count.

The threshold is static and the sweep is a hand-run `#[ignore]` test in
`app/sim_host/bench.rs`. The numbers are that benchmark's first output, not
this document's.

The sweep's budget check used to hold the tick's rule work, because a tick's
Falling granular work was capped at a fixed cell count
([0018](0018-cap-the-per-tick-grain-work.md)). [0020](0020-in-tick-drain.md)
changed that: below the rail a tick drains the whole queue, so the scene sets the
rule work and the check measures the whole tick again. Above the rail the rail
serves a fixed count a tick and the check still holds that count's work. For the
scenes this tripwire was set against, the bistro occupancy and the generated
extents, the rule work stays inside the budget and the commit window is what
crosses it. The sand ball is the new case: a full ball tick is 555 to 592 ms, of
which the drain is 437 to 475 ms, so its rule work is what crosses. A scene that
is meant to be watched while it settles is not a tripwire target, so the
threshold still applies to the scenes it was set against.

This narrows the check without narrowing the tripwire: the threshold above is
still the whole tick, and the whole tick still crosses it at the bistro
occupancy and at the larger generated extents. What
[0018](0018-cap-the-per-tick-grain-work.md) establishes is that the rule work is
inside the budget for those scenes and what crosses is the commit window's
Micro-chunk compile and tracked-set clone. The sand ball is the exception it does
not cover, since its rule work is the crossing. The dense local window that the
consequences below name as the replacement was aimed at the evaluation order, so
it would not address a commit-window crossing.

## Consequences

- The simulation is drivable headless. Nothing in it needs a frame, so the acceptance walkthroughs run without a window.
- If the tripwire fires, the preferred replacement is draining region by region into a dense local window, copied once per tile and evaluated locally, matching the existing `(y, x)` drain order. It keeps the `World` contract intact and changes no determinism guarantee, because cells still evaluate in the same order, reading from a copy fetched moments earlier. Batched neighbourhood reads fight the enqueue-on-read design, since a rule read decides what enqueues next and so is not knowable before evaluation. A new `World` access path reopens a settled contract for a gain the window gets without it.
- The tripwire names a threshold rather than a strategy, so a crossing point can be reported without a redesign already having been written.

## Status

accepted (2026-09-30). Promoted from the physics wayfinder's decisions 05, 09,
11, and 13, which were resolved against a prototype and had no earlier ADR.
Decision 13 reopened 05 and 07 wherever they conflicted. The thread and channel
boundary here supersedes the in-place `update(&mut world, ...)` call recorded in
07 and the frame-loop clock ownership in 05. Amended (2026-10-05): the tripwire
section records what the sweep's budget holds now that a tick's Falling granular
work is capped, per [0018](0018-cap-the-per-tick-grain-work.md). Amended
(2026-10-15): the clock, the staleness bound and the stalling window are restated
for a tick that costs more than a frame period, per
[0020](0020-in-tick-drain.md). The World's rate is the host clock's only while a
tick fits in a frame period, a raycast's one-tick staleness is now up to about
0.6 s on the sand ball, and a host edit waits through the tail of the evaluation
window and the commit rather than on the drain alone.
