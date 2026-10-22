# A running World can rain

A supplied World may carry an `Emitter`: the cells it plants grains into, the
interval between the ticks a cell may fire on, and the grains planted per tick.
The supply hands the emitter over with the World, `Activation` carries it into
the simulation, and the runtime plants from it inside `commit`, under the write
lock a tick already takes, after the rule edits and the queued commands. Every
planted cell is woken into the update queue, so the following tick's drain
serves it.

## Why

Every World the project could build stops moving. A Generation settles in a few
levels, a `.vox` load stays as authored, and the sand ball's whole queue is gone
by tick 128. Since [0021](0021-drain-the-whole-queue.md) a tick serves the whole
queue with no cell cap, so a tick's cost follows the grains in flight, and there
was no way to see that cost held at a steady level rather than spent once.

The supply is one-shot by design: it builds a World, hands it over, and the
rules own it from then on. The one existing way to write into a live World is a
host command, which is a message per edit and belongs to a host that knows what
it wants to change. A scene whose whole purpose is to arrive on its own has no
host to ask, and 128 messages a tick would be a worse way to write into a live
World than one field on the handover.

## Consequences

- A tick commits even when no rule edit and no command moved anything, because
  the emitter is its own reason to commit. That tick still produces a snapshot
  batch for the Micro-chunks it wrote, so planted cells reach the renderer by
  the same route a rule's move does.
- The emission is a function of the tick count and the emitter, so a run
  replays: the same build rains the same cells on the same ticks. The tick
  count resets on activation, so a reload restarts the schedule.
- An emission whose cell already holds a grain is skipped and counted. Nothing
  clears a hole, so a scene whose pile has reached the plate goes quiet rather
  than ending, which is why `UpdateReport` carries the emitted and skipped
  counts and the frame log prints them.
- The emitter is World state in the supply's sense, not sim configuration, so
  `sim::spawn` and `PlayerProfile` are untouched and a reload can change what
  rains without restarting the thread.
- The plate and the holes come from one module, `world::rain`, so an emission
  cell is always a hole and a Bedrock cell is never planted into.
- The drain's cost tripwire is unaffected: the emission adds no rule work, so
  [0021](0021-drain-the-whole-queue.md)'s bound of 600 ns per drained cell in
  `app/sim_host/bench.rs` stays as it is.
- `--no-sim` builds the plate and the floor and no rain, because there is no
  tick to plant from. The flag keeps meaning what it says.

## Considered options

- **Emitting from a host-side command each tick.** Rejected: it puts a scene's
  clock in a host that exists to render, and it is 128 channel messages a tick
  for a thing the simulation can compute itself.
- **A tank of Sand above a Bedrock sieve, built entirely at supply time.**
  Rejected: a grain enters a hole directly beneath it and cannot search
  sideways, so the tank drains only down the columns above its holes, and the
  rate is whatever the hole pattern yields rather than a number the scene sets.
- **A taller pre-placed slab that falls once.** Rejected: it is the sand ball
  with a different shape, and it stops raining, so it answers no question the
  ball leaves open.
- **An `Emitter` argument on `sim::spawn`.** Rejected: spawn is about the thread
  and the player's physics, the emitter belongs to the World being activated,
  and the per-World channel already exists.
- **Suppressing the commit on ticks with no rule work.** Rejected: it would make
  the rain arrive in bursts timed by whatever else moved, so the scene's rate
  would depend on the scene's other content.

## Status

accepted (2026-10-22). Adds the emitter to the supply and the activation, and
plants from it at commit. Leaves
[0021](0021-drain-the-whole-queue.md)'s per-cell tripwire and
[0022](0022-serve-each-tick-as-it-finishes.md)'s per-tick push unchanged.
