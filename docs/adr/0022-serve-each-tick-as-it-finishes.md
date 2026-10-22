# A tick reaches the host as it finishes

The sim pushes one `Push::Tick` per tick it runs. A tick's `UpdateReport`
carries that tick's own batch, and the update's discard count and snap flag ride
the update's first tick, the only place they mean anything. An update still runs
at most five ticks; the catch-up cap and its reason are unchanged
([0012](0012-simulation-thread-and-clock-ownership.md)).

## Why

An update that ran several ticks pushed once, with every tick's snapshot list
folded into `report.batches`. The host applied them all in one frame, so the
renderer's first rebuild after the update showed the last tick's state and none
of the intermediate ones reached the screen.

On the sand ball, 1,097,917 grains and five ticks an update under the cap, that
made the body jump five cells per visible step instead of one, and the frame
log's `processed / window` read 5,540,944 grains a second for a window that
caught one update's burst. That figure is five ticks of 1,110,768 cells, not a
rate, and it is what made a 0.6 s tick look like a 5 s one.

The batching bought nothing. The ticks are sequential, each commits before the
next evaluates, and a push is one message on a channel that already carries
readiness and activation batches.

## Consequences

- The host applies one batch per tick, so the renderer rebuilds per tick rather
  than once per update. On the sand ball that is the same 2,716 Micro-chunks
  either way, in five smaller rebuilds instead of one folded one.
- The camera interpolation advances per tick, so a frame draws the pose between
  the two newest ticks rather than between the two newest updates.
- A window of the frame log can still catch several ticks, but it now counts
  them: the line prints ticks a second, milliseconds a tick and grains a tick
  from the sim's own counters, so a burst cannot pass for throughput.
- The push is sent inside the update loop, so a shutdown between ticks ends the
  update with the ticks already pushed delivered. The check's position in the
  loop does not move.
- `UpdateReport` loses `ticks` and `Vec<Vec<MicroChunkSnapshot>>` becomes
  `Option<Vec<MicroChunkSnapshot>>`, because a push is a tick and a tick commits
  at most one batch.

## Considered options

- **Removing `MAX_TICKS_PER_UPDATE` so every owed tick runs.** Rejected: a tick
  costs 595 ms against a 33 ms period on the sand ball, so 17.8 ticks come due
  per tick of wall time and the owed count multiplies by that factor every
  update. One update would run unbounded, the frame inbox would grow without
  bound because the sim would stop reading it, and the discard count that
  reports the falling rate would never fire. The cap bounds the simulated time
  one update may carry; the per-tick push is what the display needed.
- **Pushing once per update and coalescing in the host.** Rejected: the host has
  no clock of its own for deciding when a tick is worth showing, and the ticks
  are already separate states at the boundary.
- **A wall-clock budget in place of the tick count.** Rejected for the reason
  [0021](0021-drain-the-whole-queue.md) gives: the tick's outcome would depend
  on the machine and the run.

## Status

accepted (2026-10-22). Damps
[0012](0012-simulation-thread-and-clock-ownership.md), which recorded one push
per update, and leaves the cap there unchanged.
