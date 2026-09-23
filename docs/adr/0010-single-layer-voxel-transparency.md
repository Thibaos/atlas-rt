# Single-layer transparency in the voxel ray pass

Palette alpha below 255 (Transparency) is resolved in the raygen shader as a
one-layer alpha blend: the nearest transparent surface (0 < a < 255) shades like
an opaque one and blends over the first opaque surface or the Background behind
it, `src * a + dst * (1 - a)`, using linear coverage `a / 255`. Alpha 0 is fully
see-through: the shared DDA skips it, so it never intersects. Shadow rays skip
every transparent cell, so no transparent surface casts shadow and a pane costs
no extra shadow trace.
The mechanism is a raygen re-trace loop: no any-hit shader exists anywhere, and
recursion depth stays 1 with every trace originating in raygen. Voxel mode only;
Hull and Normal stay opaque, and the output image alpha stays 1.0.

A blended pixel costs exactly two primary traces and two shadow traces (stash
point and backdrop), regardless of pane thickness. Opaque pixels take one
additional alpha branch on a register and otherwise keep their existing trace
counts: one primary plus one shadow when occluded from the sun.

## Considered options

- **Any-hit shader on the voxel group**. Rejected: per-voxel round trips through
  `reportIntersectionEXT` for every transparent cell, and `intersect.rint` re-reports
  `t == tmin` on resume, indistinguishable from the camera-in-voxel case where
  `last_axis < 0`. The payload is opaque to intersection shaders, so any-hit cannot
  stash the transparent hit for raygen either.
- **Pure raygen loop without a skip variant**. Rejected as the sole mechanism: a
  thin pane is cheap but a thick pane costs one primary trace per transparent cell,
  which is the cost any-hit loses on. The skip variant bounds blended pixels at two
  traces.
- **DDA-skip only (no stash)**. Rejected: skipping transparent cells with nothing
  to remember paints the first opaque surface as if the pane were absent.
- **Per-voxel opacity flags or a second material table**. Rejected: Transparency is
  a Palette property; the Palette is already a GPU vec4[256] and a .vox palette
  already carries alpha.
- **Tinted or attenuated shadow through glass**. Rejected: pane alpha would have
  to accumulate during shadow traversal, and the payload is opaque to
  intersection shaders, so no channel exists. One shadow trace and Teardown's
  ignore rule both argue for skipping instead.
- **Multi-layer blend or refraction**. Rejected as out of scope: one layer matches
  Teardown-class expectations and keeps the raygen loop at one re-trace.

## Consequences

- The CPU reference tracer (ADR
  [0008](0008-validation-reference-tracer.md)) must model the same single-layer
  blend and the alpha-0 skip when it is built; `.scratch/cpu-reference-validation`
  was amended from "alpha is one" to that rule.
- `get_palette` returns RGBA (`[glam::Vec4; 256]`), sRGB-encoded like the
  Palette itself, linearization still at the hit; upload sites no longer force
  alpha 1.0.
- Normal mode changes: alpha-0 voxels disappear from the heatmap, since the skip
  lives in the shared DDA. Glass (0 < a < 255) still paints solid there.
- The re-trace skips every remaining transparent cell, so a transparent surface
  behind the nearest pane never appears: two stacked panes read as one and the
  farther pane's color is dropped.
- Trace-count baseline (analytical, correctness-only acceptance): opaque pixel 1
  primary + 0 or 1 shadow; blended pixel 2 primary + 2 shadow; shadow through an
  alpha-0 gap or a pane 1 trace; Hull and Normal modes unchanged (0 or 1 primary,
  no blend).
- The dead `COARSE_SHADOW_SBT_INDEX` record, if ever wired, must adopt the
  skip-alpha intersection shader rather than the opaque default.
