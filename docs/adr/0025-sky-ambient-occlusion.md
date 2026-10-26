# A sky ambient term measured by occlusion samples

A hit's radiance becomes `A * (sun_visible + ambient)` for the Palette entry `A`,
with the sun term binary: a blocked sun contributes nothing where the quarter
multiply used to stand in for the light it could not measure. `ambient` is
measured rather than assumed:

    ambient = (1 / N) * sum_i(visible_i ? sky(dir_i) : floor * sky(normal))

`N` is a Scene field, 0 to 16, default 4, set by the `--ao` flag on the
standalone path. At zero the raygen keeps the old shading, the 0.25 multiply
included, so the look acceptance has a before image to compare against.

Samples are cosine-weighted over the hemisphere around the geometric Normal, so
the estimator needs no cosine factor and the ambient is a plain mean of what the
samples saw. The set is the R2 sequence, rotated per pixel by two 16-bit values
taken from one `hash_mix` of `gl_LaunchIDEXT.xy`, which reads only integer launch
coordinates and keeps a run reproducible; the rotation turns the residual error
into fine grain rather than the coherent patches an identical set produces. The
basis is the branchless Duff construction, because a camera-embedded hit has no
crossed face and therefore no axis-aligned normal. A sample starts at the hit
point offset 1e-3 along the normal and traces with `RAY_T_MIN` at 0.01, since a
nearly tangent ray plus float error in region-relative object space can otherwise
re-enter the voxel it left.

The traces reuse the opaque-only shadow group, SBT index 1, with `tmax` at
`ao_radius`, default 8, so no hit shader and no group is added and a transparent
voxel never occludes, matching the rule that it casts no shadow. The result
travels in a payload at location 2 holding one float: the raygen seeds it to 1.0
before each trace and the hit clears it. A sample that hits inside the reach
contributes `ao_floor * sky(normal)`, default 0.1, so an enclosed pixel is dim
rather than black, and the ambient divides by the sample count, not by the number
of samples that passed. A sample that escapes contributes the Procedural sky
along its own direction, which the raygen evaluates because it already holds that
direction.

A miss writes no payload at all, and the raygen assumes the miss before each
trace: `payload.color` starts as the Procedural sky along the primary direction
and a hit overwrites it. That is not a preference. On this driver a miss stage's
`rayPayloadInEXT` writes arrive one float early, so a sky written by the miss came
back as `(sky.g, sky.b, 1.0)` with its `t` arriving as `sky.r`, which put every
sky pixel through `shade_hit` and through the shadow multiply. Both writers of the
visibility flag are the raygen and the hit, which agree.

Voxel mode only. Hull and Normal never call `shade_hit` and carry no ambient.

## Considered options

- **A visibility factor times an ambient constant**, the classic reading of
  Ambient occlusion: `ambient = visibility * sky_irradiance(normal)`. Rejected as
  the mechanism because the fraction discards which part of the sky is visible, so
  a pit that sees only the dim horizon gets the same ambient as an open plain, and
  because a constant ambient needs the strength knob this feature went without. It
  has one real advantage, an open surface measuring exactly 1.0 and rendering
  grain-free, which was weighed at 4 and 16 samples and declined; see Consequences.
- **A gain constant on the ambient.** Rejected: this feature adds no constants,
  and the sun term's balance belongs to the lighting pass.
- **Denoising or a spatial blur.** Out of scope.
- **Sampling the sky once at the normal and scaling it by the visible fraction.**
  The same trade as the factor above.
- **Letting the miss write the radiance into location 2**, the first
  implementation. Reverted after the payload finding above: the raygen owns the
  ray direction, so the miss had nothing the raygen lacked.
- **Carrying the collision result in the existing shadow payload at location 1.**
  Rejected: the occlusion trace passes location 2, and a write to a location other
  than the traced one does not reach the caller on this driver, so the sun's
  payload cannot report an occlusion hit.

## Consequences

- Cost: `N` extra traces per hit and `2N` on a blended pixel, against the primary
  and sun traces the pass already took. This amends
  [0010](0010-single-layer-voxel-transparency.md)'s trace-count baseline.
- An open surface is still noisy at four samples. The sky runs from 0.6 to 1.2
  across the hemisphere, so four rays estimate its mean to about seven percent,
  and that is the grain visible on a flat top face. At 16 it is nearly gone.
  Nothing about it is occlusion, which is also why it covers surfaces no occluder
  can reach.
- The ambient's colour is the sky's, so `A * ambient` shifts warm albedos toward
  it: sunlit sand converges red and green, and shadowed sand lands grey-green
  because the sky's green to red ratio exceeds the sand's red to green.
- The CPU reference tracer of [0008](0008-validation-reference-tracer.md) must
  reproduce the same estimator, the R2 set, the pixel hash and the floor rule, or
  the ambient compares as a divergence. The sampling is deterministic, so an exact
  match is possible in principle. It stays out of scope for this feature.
- `--ao 0` is temporary: the branch keeps the 0.25 multiply and is deleted when the
  lighting pass settles the sun term.
- Miss stages write no payload anywhere in the pipeline, so `miss.rmiss` is an
  empty shader and the sky lives in the raygen.

## Status

accepted (2026-10-26). Records the shading model, the sampling and the trace reuse
from `.scratch/ambient-occlusion/spec.md`, judged at 1920x1080 on the standalone
path. Leaves [0002](0002-ray-pass-output-contract.md) and
[0006](0006-offscreen-output-contract.md) in force: the ray pass still writes
linear radiance into the delivery image and reads no depth from it. Amends 0010's
trace-count baseline, which now counts the occlusion traces.

The spec's sentence that the miss gains a payload carrying sky radiance is
implemented with the raygen evaluating that radiance instead, for the reason
above; the payload at location 2 exists and carries the visibility.

Restates **Procedural sky** and **Background** in `GLOSSARY.md`, adds **Sky
ambient** and **Occlusion sample**, and documents `--ao` in `README.md`.
