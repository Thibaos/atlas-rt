# Player contact is swept, and support and stepping read the whole footprint

The Player collider moves by swept test, resolved horizontal before vertical.
Support and head clearance both read the collider's full footprint rather than
its center, and an automatic step is refused as a whole or not at all. The
[Player profile](../CONTEXT.md) owns every dimension and motion value, and this
record deliberately does not restate them.

## Considered options

- **Move-and-test contact**. Rejected against a simulation. It advances to the target pose and then depenetrates, so the player ends up floating above the floor, 0.84 in the shaft scene, reports not grounded, and swallows the jump input. Swept contact lands the feet exactly on the surface with zero overlap.
- **Feet-first axis order**. Rejected on the same evidence. Horizontal first ends a clipped ledge lip with the feet on top of the surface at 7.000, where feet first jams the body against the side at 6.973. Axis order matters only at edge contacts, and wall sliding, ceiling contact, and the corner cases fall out of the two-axis split without special handling.
- **Sideways depenetration**. Rejected. Depenetration lifts up first, because gravity is the reason the player sank in. Buried with no escape path keeps the overlap and keeps moving, and depenetration retries every tick while sweeps skip cells already inside the box, so the player can still shuffle sideways out of a cell behind them.
- **Center-cell or half-footprint support**. Rejected against a simulation. Both let go of a ledge earlier, at ticks 45 and 46 against 48, and both flick grounded off for a tick mid-climb, which spends jump input.

## Support

A blocking cell supports the player when any cell under the footprint has its
top within contact tolerance of the feet. Footprint-wide support is the rule
because a narrower one drops support while part of the collider still rests on
something.

A gap counts as ground unless it is two cells or wider. On an integer lattice
the smallest gap is one cell, 1.0, which is wider than the collider, so
"strictly narrower than the collider width" would swallow every crack in a
floor. A one-cell crack is walkable and reports as bridged. A two-cell trench
drops.

The bridge is not only a flag. The downward sweep must be skipped while the
support rule holds, or gravity drops the player through the gap the rule just
called ground. The prototype caught exactly that bug.

## Stepping

A step fires on horizontal contact while grounded only. Never in the air,
never before contact, and no automatic step down, so walking off an edge falls
under gravity and the far side of a pile is a clean drop.

Head clearance uses two boxes, and the step is refused whole rather than
committed and cleaned up after.

1. The rise box, the player footprint as tall as this step's actual rise, sits on top of the head at the current position. It catches the ceiling the body rises through.
2. The full body box at the destination must also come to rest on something.

Skipping both was compared: the player rises, has nothing under the feet, floats
back down, lands, and repeats, never crossing. Sizing the rise box with the full
Step height instead of the actual rise was rejected, because it demands the step
height plus the body height in headroom before any step at all, which observed
as 3.8 voxels against a step height of 2, and it refuses low steps that would
have fit.

A step surface is any blocking cell, so a settled granular pile is climbed one
rise at a time. The rise comes from the top of the contiguous blocking stack in
the contacted column, so stepping onto a two-cell platform rises two in one
move, and a stack taller than the Step height is a wall. A Step height of zero
disables stepping.

## Profile ownership

The profile belongs to the controller rather than to the `World`, so loading or
clearing a World does not change it. A different profile requires a new
controller or an explicit controller rebuild. The validated constructor rejects
invalid values instead of clamping them, so an impossible profile fails to
build rather than silently becoming a different one. One voxel equals one world
unit.

The constants themselves are the code's business and the prototype will retune
them. What this record fixes is the ownership, the reject-don't-clamp policy,
and the shape of the collision and support rules the constants feed.

## Lattice edges

Any cell outside the half-open ±2048 lattice counts as blocking for the
collider. The sides are an invisible wall, the top an invisible ceiling, and the
bottom an invisible floor whose surface plane is y = -2048. Wall sliding and
grounding use the existing contact path unchanged, and the controller never
issues an out-of-lattice read, which would panic.

## Placement and spawn

On activation the feet go to the bounding-box center on xz, on top of the
highest occupied cell in that column, with vertical velocity zeroed. Yaw,
pitch, and the profile survive. An empty column puts the feet at the top of the
world bounding box at that xz, and gravity settles the fall. An empty World
puts the feet on the lattice floor with no fall.

A player who starts inside solid cells is pushed up cell by cell by
depenetration until the collider is clear or the lattice ceiling. With no clear
position the player stays buried, where the buried rule already keeps the
overlap and still lets the player move. There is no sideways search, so no new
deterministic order has to be specified.

## Consequences

- Grounded flicking costs a jump press, which is why the support rule reads the footprint rather than a single cell.
- Stepping is all-or-nothing, so a refused step leaves no partial rise to clean up.
- The verified behavior is a set of simulation checks, not a frame budget. The walkthroughs drive every support rule, the three step heights, the one-cell bridge and two-cell fall, the ledge drop ticks per rule, both refusal boxes, the skipped-check bob, and the pile climb and jump.

## Status

accepted (2026-09-30). Promoted from the physics wayfinder's decisions 02, 03,
04, and 10, which were resolved against a prototype and had no earlier ADR.
Only the ownership rule and the reject-don't-clamp policy carry over from 02. Its
constant table is not restated here and belongs to `PlayerProfile` in the code.
