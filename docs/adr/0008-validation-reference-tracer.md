# Independent CPU reference tracer for validation

The renderer's correctness is validated against a naive per-voxel CPU ray tracer
over the World's own voxels, an algorithm independent of the renderer's
representation. It shares only the camera inputs and the Palette with the GPU
path, so a divergence points at the renderer rather than at assumptions both
sides hold. The reference drives the scene from `World::get_voxel` and knows
nothing about Micro-chunks, Snapshots, hulls, region pools or the BLAS.

Each pixel's `{color, t, normal}` is compared against a captured frame of the
same camera. This ADR states the comparison surface, the tolerances and the two
geometric excuses. The Shadow term and the capture surface are written into this
document by ticket 02, which owns them, and the reference implements whatever
those rules state.

## Status

accepted (cpu-reference-validation ticket 01, 2026-09-17). Restores the decision
deleted in `e78b5ff` along with the validator, adapted to the current scope. That
decision was numbered 0003, a number later reused by
[0003](0003-renderer-input-contract.md), which names the renderer input contract
rather than a validation reference; citations of "ADR 0003" in a validation
context mean this record.

The path-traced comparison the deleted validator also carried stays out of scope.
Sample means over N seeds, seed evidence and the path excuse classes (`Firefly`,
`FaceTie`, `PathDivergence`) went with the path tracer, the NRD denoiser, the
radiance cache, the material table, sun/sky MIS and the engine-side Composite,
all removed in September 2026. Nothing here revives them.

## Considered options

- **Naive per-voxel tracer over the World's voxels**. Chosen: per-pixel
  ray-vs-voxel stepping in world space, with none of the renderer's structures:
  no DDA, no AABBs, no pools. Validates the whole renderer path (Micro-chunk
  snapshots, region pools, trimmed hulls, BLAS, the intersection DDA, `hitKind`,
  Palette) against an implementation that shares none of it.
- **DDA mirror over the renderer's own representation**. Rejected as the
  reference: the same algorithm on CPU, so it isolates GPU-implementation bugs
  but shares every algorithmic assumption. Kept as the mirror escalation path:
  when a diff appears, a mirror over `RegionData` localizes it to GPU versus
  algorithm.

## Scope

In scope: primary visibility through the DDA, the hull AABB test, the Palette
lookup, the entered-face `t`, the reconstructed normal, the Shadow term, and the
Procedural sky on a miss. Monte Carlo and path-traced comparison are out of
scope, as are the Godot transport and the engine-side Composite, since the
validator renders offscreen on atlas-rt's own device.

The ray parameter range matches the camera (`t_min` = near, `t_max` = far), so
both sides clip the same way, as [0002](0002-ray-pass-output-contract.md)
records.

## Comparison

- Color is linear radiance and compares as floats under a small absolute
  tolerance, with both sides reading the same Palette. Ticket 06 settles the
  value when it writes the comparison; no code path fixes it today, since the
  exact-u8 rule of the deleted validator went with the UNORM swapchain.
- `t` matches when `|dt| <= 1e-3 * max(|t_gpu|, |t_ref|, 1)`.
- The reconstructed normal compares as floats on the same rule as color.
- A world passes when hard mismatches stay inside a mismatch ratio of 0.01, one
  percent of pixels.

Two geometric excuses. A color mismatch on the one-pixel-dilated silhouette of
the reference image is excused. A color match within an absolute `t` distance of
2.0 is excused as `corner_touch`, because a corner-grazing ray can commit the
neighbouring voxel at unit scale, where that distance is about two voxels, and a
visibility or depth bug moves `t` by far more.

## Consequences

- The reference tracer and the renderer are deliberately allowed to disagree in
  the middle. Only the per-pixel result is compared.
- The comparison runs offline on demand, one frame per camera, with no warmup
  and no frame budget: the reference is not accounted against 16 ms.
- The reference must be updated whenever the shading it validates changes,
  because a reference that lags the renderer reports the renderer's intended
  change as a divergence.
