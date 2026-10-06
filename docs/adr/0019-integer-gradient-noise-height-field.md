# The Height field is integer gradient noise keyed by the Seed

The Height field is gradient noise over four octaves, at lattice spacings of 128,
64, 32 and 16 voxels and with each octave half the height of the one before. Each
lattice corner carries one of eight unit directions, hashed from the Seed, the
`Feature::Terrain` tag, the octave index and the corner's two coordinates, so
every column that reads a corner agrees on its gradient. The arithmetic is Q16
fixed point with `i64` intermediates, and `lerp` is the one operation that rounds:
it adds half before its shift and rounds half up. The surface stays quantized to one
level per column over the same 65 levels, ground level 0 and Bedrock at -64, so
the fill and the depth layering are unchanged in kind.

The Seed is a required argument of the height function, `surface_level(seed, x,
z)`, which reads the Terrain tag itself. Commit 18a0cbf replaced an already-unused
`seed` argument with the constant Terrain tag, and the field went on building one
World for every Seed, voxel for voxel, for the whole life of the Height field.
Requiring the Seed cannot stop a later author from ignoring it, because an ignored
parameter still compiles; what keeps it read is that nothing else supplies the
World's variation.

## Status

accepted (2026-10-05). Records the noise chosen for the Height field by
`.scratch/terrain-shape/`, which implements the rejected-float option of
[0017](0017-procedural-world-generation.md) rather than contradicting it.
Amends **Height field** and **Seed** in `GLOSSARY.md`, and cross-references 0017.

## The measurements behind the shape

`HEIGHT_SCALE`, the levels one unit of noise is worth, is 25, measured by the
ignored `the_surface_distribution_chooses_the_height_scale` and then chosen from
its output, which a cross-run at scale 27 settled: a standard deviation of 10.861
levels at 27 makes the noise worth 0.402 levels per unit, and 10 / 0.402 rounds
to 25.

Over a 512-edge window of 262,144 columns it measures min -32, max 30, mean
2.512, standard deviation 10.291, 41 columns at the floor (0.016%), none at the
ceiling, a mean adjacent step of 0.335 and 39.6% of columns below ground level.
Over the full Lattice, 16,777,216 columns, min -32, max 32, mean -0.398, standard
deviation 10.058, 0.008% clamped at the floor and 0.004% at the ceiling, a mean
adjacent step of 0.332 and 49.5% below ground level. The clamp fires on under
0.02% of columns at either edge, so the surface's range is set by the noise
rather than by the clamp, and ground level 0 stays the surface's average within
0.04 of a standard deviation.

The 512-edge window reads 2.3% above the full Lattice on spread and 2.5 levels
high on its mean, because that window holds only four by four cells of the base
octave. The full-Lattice figures are the ones the scale was chosen against.

Coherence, over a 64-edge extent at seed 0x5EED_1234: the worst adjacent pair
of columns differs by 2 levels against a mean of 0.351, and a sweep of 24 seeds
at extent 16 and extent 64 reaches no pair above 2. The white noise field's
mean adjacent step was 21.7, so its ground changed level at almost every column
and had no landform at any scale.

## Considered Options

- **Float noise.** Rejected by [0017](0017-procedural-world-generation.md): the
  pinning promise that one Seed fixes one World on every machine and every build
  would otherwise rest on the toolchain's floating-point behaviour rather than on
  the Seed alone. The noise is therefore computed in fixed point, so the promise
  stays a property of the Seed.
- **Value noise**, which interpolates a scalar at each lattice corner instead of
  a gradient. Rejected: it reads as blobs rather than hills, because the surface
  has no preferred direction inside a cell and flattens at every lattice point.
- **A single octave.** Rejected: it has no detail on the slopes and no variety at
  a scale larger than its own spacing, so a 128-voxel lattice reads as a smooth
  dune and a 16-voxel one loses the landform.
- **An unscaled `{-1, 0, 1}` gradient set.** Rejected: it is simpler, but the
  diagonals come out `sqrt(2)` longer than the axes, so the amplitude depends on
  the direction a cell happens to face. The table scales each diagonal by
  `1/sqrt(2)` in Q16, which brings every direction to the same length.
- **A 256-entry permutation table shuffled from the Seed.** Rejected twice over:
  it is a structure the generator does not need, because a corner hash can be
  keyed directly from the coordinates it reads, and it repeats every 256 lattice
  steps unless the corner's own coordinates are folded in, which is a period a
  field spanning the Lattice cannot carry.
- **Keeping white noise**, the per-column hash that reduces a column's x and z
  modulo 65. Rejected: neighbouring columns are independent draws, so the ground
  changes level at almost every column and carries vertical walls instead of
  landforms. Its mean adjacent step is 21.7 levels, and because about half its
  columns sit below ground level and carry a Sand surface cell, the Falling
  granular rule avalanches that Sand into the pits, so the surface the renderer
  shows is not the surface the generator wrote.

## Consequences

- The height function costs about 100 ns per column where the white noise field
  cost one `splitmix64` chain, and a full-Lattice Generation runs at 5.485 s
  through the World job, generate 4.134 s plus emit 0.963 s, inside the
  8-second budget. The corner hashes are not hoisted per Micro-chunk:
  `column_surfaces` computes a chunk's 64 columns once and the fill reuses them
  across the chunk layers above, so the per-column cost is not paid per layer.
- A corner is hashed from its own coordinates rather than reached by an offset
  from its neighbour, which is what lets columns that share a corner agree on it.
  Folding the octave index into the hash decorrelates the octaves, so there is no
  per-octave offset table.
- The lattice spacing is a power of two, so `div_euclid` is an arithmetic shift
  and `rem_euclid` is a mask, and a negative x or z lands in the cell below zero
  instead of wrapping into the one above it.
- The coherent fill leaves whole Micro-chunk layers empty above the surface, so
  the full Lattice's fill writes 2.30M entries where the white noise field wrote
  3.31M. The Generate stage's share of the pipeline moved with it and was
  re-derived: `GENERATE_STAGES` is 829,000 and 830,000.
- The surface barely avalanches. At the full Lattice the white noise field moved
  55% of its queue in the first tick where this field moves no grain, and the
  whole settle moves 467,607 of 8,477,286 grains, 5.5%. The grain count is
  unchanged at 50.5% of columns, because the level distribution is unchanged.
- `the_surface_is_coherent`, `two_seeds_differ` and `a_seed_pins_a_world` in
  `crates/atlas-rt/tests/generation.rs` guard the shape, the Seed and the
  rebuild. The fingerprint `a_seed_pins_a_world` checks in is rewritten only when
  this function changes on purpose.
