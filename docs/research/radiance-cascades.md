# Radiance cascades

> Verification date: 2026-11-14. Sources: Sannikov's paper
> (`Raikiri/RadianceCascadesPaper`, WIP, never published in a journal), Split
> Radiance Cascades (arXiv 2607.20384v2, 2026-10-11), Holographic Radiance
> Cascades (arXiv 2505.02041), Osborne and Sannikov (arXiv 2408.14425, published
> in RASTI), the ExileCon 2023 talk listing and GGG forum post, the erhe plan
> document, `three-rc`, `src-dgi`, the `M3ycWt` shader source (read in full), and
> the community write-ups in Sources. Sections 5.2 to 5.4 and all of section 6 are
> local analysis, labelled [A].
> The original article URL (`aleksander.sannikov.com/2024/08/09/radiance-cascades/`)
> no longer resolves: the domain now serves a parking page and Wayback has no
> snapshot. The paper is the authoritative text.

Evidence labels below: **[P]** first party (the technique's authors or the
implementation's own authors), **[S]** secondary (third-party explanation or
community report), **[A]** my analysis, not from a source.

## Summary

A radiance cascade is a hierarchy of probe grids over one scene. Cascade `i`
stores only the radiance arriving from a distance interval `[t_i, t_{i+1}]`, with
probe spacing `2^i` times cascade 0 and angular resolution `2^i` times finer, so
that each level satisfies the penumbra condition for its own distance range.
Because intervals merge exactly with a premultiplied-alpha rule, all levels can
be collapsed into cascade 0, which then holds a cone-averaged full radiance
field. The total data is bounded by twice cascade 0 for any number of levels, and
the number of reconstructable rays grows as `2^N` while cost grows as
`1 + 1/2 + ... < 2`. That is the "infinite" claim: unbounded distance at bounded
cost, not unbounded angular or spatial resolution.

Path of Exile 2 uses a screenspace, flatland-like cascade hierarchy with
screenspace raymarching and a 128-bit occlusion mask per texel, stated by its
author to cost about 3 ms on a GTX1050. That is the only first-party account of
the shipped implementation, and it is screenspace, not volumetric.

Full 3D is where the technique strains: cascade 0 is a dense volume grid, so its
cost sets the floor, while lower cascades dominate the work. The 2026 answer is
sparse storage. Split Radiance Cascades stores world-space probes in a hashmap
near visible surfaces and reports 8.6 ms (Sponza) and 11.5 ms (San Miguel) on an
RTX 3080 Laptop for one bounce, single frame. No published implementation stores
cascades in an SVDAG or brickmap; that mapping is reasoned about in section 5 and
labelled as analysis.

Storage is a per-level region with a per-level probe format, never a mip chain
(section 5.3). World size scales with probe count rather than extent, and tracing
binds before memory does (section 6).

## 1. What radiance cascades are

### 1.1 Penumbra condition and hypothesis

Take a light source of linear size `w` at distance `d` from an infinite half
plane. The penumbra it casts has angular width `alpha = 2 atan(w / 2d) ~ w/d`.
To resolve that penumbra at distance `D` from the source, the probe spacing must
be smaller than the penumbra's linear width there, and the angular step must be
smaller than the source's angular size there:

$$
\Delta_p < \alpha D, \qquad \Delta_\omega < w/D
$$

Holding `w` and `d` fixed gives the **penumbra condition**:

$$
\Delta_p \lt\sim D, \qquad \Delta_\omega \lt\sim 1/D
$$

where `A <~ B` means "A is less than some function that scales linearly with B"
[P, paper 2.1]. The **penumbra hypothesis** is the assumption that satisfying this
condition is necessary and sufficient for a discretization to represent the field
at every scale [P, paper 2.2]. Two extreme cases satisfy it: ambient occlusion
(`Δp → 0`, `Δω → ∞`) and environment mapping (`Δp → ∞`, `Δω → 0`). The paper
states the hypothesis fails for infinitesimally small sources placed
infinitesimally close to occluders, and reports that it holds for practical
scenes otherwise.

### 1.2 Radiance interval

A radiance interval is incoming radiance restricted to a distance range:

$$
L_{a,b}(\mathbf p, \hat\omega) =
\begin{cases}
L_{\text{out}}(\mathbf R(\mathbf p + a\hat\omega, \hat\omega), -\hat\omega)
& \text{if } |\mathbf R(\mathbf p + a\hat\omega,\hat\omega) - \mathbf p| < b \\
0 & \text{otherwise}
\end{cases}
$$

with `R` the nearest-opaque-surface raycast, plus a paired transparency
`β_{a,b}(p, ω) ∈ [0,1]` which is 0 when the segment is blocked and 1 when it is
empty. The volumetric form integrates emission through transmittance over
`[a,b]` [P, paper 2.3]. The important structural point: an interval deliberately
ignores everything nearer than `a`, which is what makes intervals from different
distance ranges independent and therefore shareable between probes.

### 1.3 The three algebraic identities

Adjacent intervals merge exactly:

$$
L_{a,c} = L_{a,b} + \beta_{a,b}\, L_{b,c}, \qquad
\beta_{a,c} = \beta_{a,b}\, \beta_{b,c}
$$

which Split RC notes is identical to premultiplied-alpha compositing with
`α = 1 - β` [P, paper 2.3.1; P, arXiv 2607.20384 §3.2].

Intervals shift along their own direction:

$$
L_{a+x,\,b+x}(\mathbf p, \hat\omega) = L_{a,b}(\mathbf p + x\hat\omega, \hat\omega)
$$

Combining shift with merge extends an interval with a copy of itself:

$$
L_{a,\,a+2x}(\mathbf p,\hat\omega) =
L_{a,\,a+x}(\mathbf p,\hat\omega) +
\beta_{a,\,a+x}(\mathbf p,\hat\omega)\,
L_{a,\,a+x}(\mathbf p + x\hat\omega, \hat\omega)
$$

This is the mechanism that lets a renderer trace a short segment and reach
arbitrary distance by repeated doubling in O(1) per doubling. The paper restricts
it to rectilinear probe grids (flatland and 3D world space), because the shifted
sample must be readable as another probe of the same cascade. It does not apply
to screenspace cascades where every probe carries its own depth [P, paper 2.3.2,
2.3.3, 4.1].

### 1.4 Cascade definition and scaling

One cascade is a linearly and directionally uniform discretization of
`L_{[a,b]}`. To satisfy the penumbra condition its steps must obey

$$
\Delta_p \lt\sim \min(a,b) = a, \qquad
\Delta_\omega \lt\sim \min(1/a, 1/b) = 1/b
$$

A hierarchy is a sequence of intervals with `t_0 = 0`, `t_{i+1} > t_i` and
`lim t_i = +∞`. The paper picks exponential probe spacing, "similar to how
mipmapping works", and derives the rest from the condition:

$$
\Delta_p \sim 2^i, \qquad \Delta_\omega \sim 2^{-i}, \qquad t_i \sim 2^i
$$

So level `i` covers twice the distance of level `i-1`, with half the linear
density per axis and twice the angular density per axis [P, paper 2.4]. In 2D
that is 4x fewer probes and 2x more directions per level; in 3D it is 8x fewer
probes and 4x more directions. `Radiant 2d` and most 2D implementations instead
use 4x more directions per level, which keeps every cascade the same size and
which Sannikov reports looked better per unit cost, without claiming to know why
[P, paper 4.1].

Because the discretization satisfies the penumbra condition, cascades are
linearly interpolateable in both space and direction without bilateral filtering
or disocclusion logic. That is the difference from ordinary radiance probes,
which need disocclusion handling precisely because they store full radiance
[P, paper 2.4].

### 1.5 Memory

2D: `P_i ~ 2^{-2i}` probes, `Q_i = 2^i` directions each, so `M_i ~ 2^{-i} M_0`
and the whole hierarchy costs

$$
\sum_i M_i = M_0 (1 + 1/2 + 1/4 + \dots) = 2M_0
$$

3D: `P_i ~ 2^{-3i}`, `Q_i ~ 2^{2i}`, so `M_i ~ 2^{-i} M_0` again, and again
`2M_0`. Surface radiance in 3D (a 2D domain with a 2D direction space) gives
`M_i = M_0` per level, so `N·M_0` for `N` levels [P, paper 2.5.1 to 2.5.3]. The
asymptotics are the same for flatland and 3D; only the constant `M_0` differs,
and `M_0` is the whole problem in 3D.

### 1.6 The resolve step

Merging runs top down. The top level merges with sky radiance, then each level
merges with the level above interpolated to its own probe positions and averaged
over the child directions of each texel [P, paper 2.6; P, arXiv 2607.20384 §3.2;
P, erhe plan §5].

For **diffuse** light, all levels are collapsed into cascade 0 and the integral is
evaluated with cascade 0's few directions. The bias is small because the
cosine-weighted BRDF term has low directional frequency, so averaging radiance
over wide cones is enough. In the 2D case each merge step is 4 texel fetches plus
a 4:1 direction average per output texel; in 3D it is 8 probes and a 4:1 average.

For **specular** light the levels are not collapsed. A cone of any width is
produced by querying one interval from each of levels `0..K`, merging those, then
merging with a single query of the already-merged level `K`. Narrow cones use
high `K` (angular step of level `N`), wide cones use `K = 0` (angular step of
level 0), and every intermediate width is reachable. Cost is O(N) per query
[P, paper 2.6].

Merge order can be front to back or back to front with identical results, the
same way volume compositing commutes [P, paper 2.3.1].

### 1.7 Why the ray count grows for free

Cascade 0 holds `P_0 Q_0` intervals. Cascade 1 holds `P_0 Q_0 / 2` more, in a
longer range. Interpolating level 1 onto level 0's probe positions and level 0
onto level 1's directions yields `2 P_0 Q_0` full-range intervals at the angular
resolution of level 1, not the `1.5 P_0 Q_0` that the stored counts suggest. Each
further level costs half as much as the previous one and doubles the number of
maximum-range intervals:

$$
\text{rays}(N) = 2^N \text{rays}(0), \qquad
\text{cost}(N) = (1 + 1/2 + \dots + 1/2^N)\,\text{cost}(0) < 2\,\text{cost}(0)
$$

[P, paper 3]. Two caveats the paper states itself: the number of levels cannot
exceed `log(P_0)` because the grids run out of probes, and the construction is
only valid while the penumbra hypothesis holds. The "infinite rays" claim is
asymptotic, not literal.

## 2. Why it is "infinite" in spatial extent

The claim rests on three properties, in order of importance.

1. **The interval sequence is unbounded.** `t_0 = 0` and `t_i → +∞`, so level
   `i` reaches `2^i` times further than level 0. Nothing in the construction caps
   distance; the last level's misses are resolved against sky or an environment
   map, which is why the top level merges with sky rather than with nothing
   [P, paper 2.4, 2.6; P, erhe plan §5].
2. **The cost of the tail is geometric and bounded.** Each added level costs half
   the previous one in both memory and work, so arbitrary distance costs less
   than twice cascade 0 [P, paper 2.5, 3].
3. **Long rays are assembled, not traced.** With interval extension, tracing
   `[t_i, t_{i+1}]` only requires tracing a short sub-interval and doubling
   `log` times, so the trace cost per level does not grow with the interval
   length [P, paper 2.3.3, 4.1].

What this does not mean:

- Not infinite resolution. The far field keeps only angular detail, and the
  angular step at level `i` is fixed by `2^{-i}`.
- Not free memory in 3D. The bound is `2M_0`, and `M_0` is a dense 3D grid.
  The paper calls this "practically equivalent to voxelizing the entire scene"
  and a dealbreaker for large scenes [P, paper 6].
- Not exact. The far field is reconstructed from interpolated intervals, and the
  hypothesis fails for very small or very sharp emitters.

Practical level counts, as reported:

| Source | Rule | Count |
|---|---|---|
| Split RC [P] | `l = 4` length scaling, 4 or 5 levels span the scene | 4 in their tests |
| jason.today [S] | `ceil(log_base(diagonal / base_length)) + 1` | 5 to 6 for a 300x400 canvas, base 4 |
| erhe [P] | until the top level has at most 2 probes on its longest axis, or a cap | configurable |
| GM Shaders [S] | `ceil(log4(diagonal / interval0)) - 1` for the 4x scheme | scene dependent |

A single extra level is often added because the analytic count lands just short
of the screen diagonal [S, jason.today].

## 3. Path of Exile 2 and Grinding Gear Games

### 3.1 Documented, first party

- The paper's author line is "Alexander Sannikov, Grinding Gear Games", and the
  paper states: Path of Exile 2's GI is "the main implementation of this work",
  using "a hierarchy of screenspace radiance probe cascades that are populated
  with screenspace raymarching" [P, paper 1, 4.2].
- The encoding is flatland-like, that is, probes scale as `2^i x 1` texels rather
  than `2^i x 2^i`. The paper contrasts this with the alternative "screen space
  probes with world space radiance intervals" variant, which is 1 to 2 orders of
  magnitude slower but captures offscreen light [P, paper 4.2, 4.5].
- Cost is stated as about 3 ms on a GTX1050 for indirect light from a glowing
  character [P, paper 4.2, figure caption].
- The raymarcher "raymarches 4 rays simultaneously, basically for free and stores
  occlusion in a 128-bit bitmask, that is equivalent to storing 128 binary
  occlusion rays per radiance cascade texel". This is the only published
  description of the inner loop [P, paper 4.2].
- Branching factor is `K = 2` per cascade, not 4, and per the 2026 follow-up
  paper that only works because the implementation traces the depth buffer, uses
  a fixed top-down camera, and the scene's vertical extent and complexity do not
  grow with distance [P, arXiv 2607.20384, footnote 1].
- First public presentation: ExileCon 2023, "Rendering Path of Exile 2",
  November 2023 [P, YouTube `TrHHTQqmAaM`; GGG forum post 2023-11-01].
- Acknowledged limitations, from the author: no offscreen emitters, invisible
  occluders do not block, no geometry behind the depth buffer. The paper is
  explicit that these come from raymarching screenspace data, not from storing
  cascades in screenspace [P, paper 6].

### 3.2 Not documented

No slides, no GDC talk, no GGG dev diary, no shader source, and no parameter dump
were found. The GDC Vault 2026 index and web searches surface nothing from GGG on
this topic. Specifically unverified: number of cascades, probe spacing, direction
count, the bitmask's angular sampling, memory, per-pass timing, and whether the
shipped 0.1/0.2 builds match the 2023 talk or the 2024 paper's description.

The talk video is the only first-party artifact besides the paper, and its spoken
content is not transcribed in any source I found. Treat any specific claim about
the talk's contents that is not in the paper as unverified.

The game does expose a "Global Illumination" graphics option in the shipped early
access build, evidenced by bug threads on the official forum (for example
`pathofexile.com/forum/view-thread/3781539`). That confirms the feature ships. It
does not confirm internal parameters.

### 3.3 Secondary claims, and one that is wrong

- A widely repeated Hacker News comment states that in PoE2 radiance cascades are
  "used in full 3d". The author's own paper says screenspace with flatland-like
  probes. The comment is wrong, or is describing a later unannounced version. Do
  not repeat it [S].
- The "3 ms" figure is often quoted as a frame time. The paper's caption says the
  figure shows the GI pass cost on a GTX1050, not total frame time.
- Predecessor context that is documented: Sannikov's earlier Path of Exile GI
  used voxel cone tracing's mipmap averaging, and the paper credits that lineage
  [P, paper 1.2]. The 2019 ExileCon talk "Evolving Path of Exile's Renderer" is
  the earlier presentation of that work.

## 4. 3D applicability

### 4.1 The dimensional scaling problem

The algebra is dimension-independent (section 1.5), but the constants are not.
In 3D, `M_i ~ M_0 2^{-i}` still holds, and the hierarchy still costs `2M_0`. The
problem is `M_0 = (L/Δp_0)^3 · Q_0` for a scene of size `L`. Halving `Δp_0` in
3D multiplies cascade 0 by 8 while the tail stays proportional, so cascade 0
dominates everything and the tail is cheap in exactly the regime where cascade 0
is already unaffordable.

Split RC states the same thing in cost form. Total work for one bounce is

$$
\sum_{n=0}^{N-1} \frac{V\,|\Omega_n|}{(\Delta s_n)^d}
= \frac{V\,|\Omega_0|}{(\Delta s_0)^d} \sum_{n=0}^{N-1} (K/2^d)^n
$$

With `d = 3` and `K = 4`, `K/2^d = 1/2`, so lower levels dominate and most of
their probes land in empty space. With `d = 2` and `K = 2` the ratio is `1/2`
too, which is why 2D RC is comfortable and 3D RC is not [P, arXiv 2607.20384
§3.2.1].

Two ways out are documented:

- **Sparse probes near surfaces.** Allocate only probes near visible geometry,
  with per-probe LOD by distance. Probe count then stays roughly constant with
  scene size and level count [P, arXiv 2607.20384 §4, §4.1].
- **Probe-on-surface instead of volume.** Surfels (SRC-DGI), screen-space probes
  on the depth buffer, or texture-mapped probes reduce the domain from 3D to 2D
  [P, `mxcop/src-dgi`; P, paper 4.4, 4.5; P, Mathis 2024].

### 4.2 What implementations exist, with provenance

| Implementation | Space | Probe storage | Tracer | Reported cost |
|---|---|---|---|---|
| Radiant 2d, Sannikov [P] | 2D | texture atlas | SDF raymarch | ~30 ms on GTX 3060, ~12 ms in the scaling figure (the paper gives both) |
| PoE2 GI, GGG [P] | screenspace, flatland-like | texture atlas | screenspace raymarch, 128-bit mask | ~3 ms on GTX 1050 |
| Radiance 3d, Sannikov [P] | world 3D | volume texture, 3D voxelization | voxel raymarch | not reported |
| Screen probes, world intervals, Sannikov [P] | screenspace probes, world rays | octahedral `2^i x 2^i` per probe | SDF raymarch | 30 to 50 ms on RTX 3060 |
| Direct radiance field renderer [P] | surface cascades | precomputed | none at runtime | under 0.5 ms on RTX 3060 |
| Mathis, Shadertoy `X3XfRM` [S] | 3D | probes in 2D textures | not read | not reported |
| `M3ycWt`, Mathis [P] | world voxels, 32x32x48 | per-level atlas row ranges, 6 hemispheres per voxel | 128-step voxel DDA | not measured here, 5.14M rays/frame |
| `three-rc`, Cody Bennett [P] | screenspace probes, world BVH | fixed-size texture | three-mesh-bvh | not reported |
| SRC-DGI, mxcop [P] | surfels | surfel probes | ray tracing | not reported |
| Split RC, Freeman, Sannikov, Pitt-Francis [P] | world 3D, sparse | hashmap + indirection buffer | OptiX | 8.6 ms Sponza, 11.5 ms San Miguel, RTX 3080 Laptop |
| erhe, Suoranta [P] | world 3D, dense grid | per-cascade atlas, octahedral tiles | Vulkan ray query | 1.15 to 1.57x DDGI at equal settings |
| Holographic RC [P] | 2D only | texture atlas | ray intervals | 1.85 ms at 512x512, 7.67 ms at 1024x1024, RTX 3080 Laptop |
| Love and Deepspace, Infold [S] | mobile 3D | not read | not read | not reported here |

Split RC's own framing: before it, "there have been three fully-3D variants of RC
for global illumination", namely Mathis (probes in 2D textures), Surfel RC, and
Love and Deepspace (GDC 2026) [P, arXiv 2607.20384 §2]. That is the useful
provenance list. Holographic RC is 2D and its authors state that extending the
same trick to 3D would need `O(x^4)` memory for a scene `x` times the base probe
spacing in size [P, arXiv 2505.02041, as summarized in 2607.20384 §2].

Split RC's measured breakdown, single frame, RTX 3080 Laptop, in milliseconds:

| Stage | Sponza | San Miguel |
|---|---|---|
| Initialize probes | 1.2 | 1.3 |
| Generate rays | 0.4 | 0.6 |
| Trace rays | 1.3 | 4.5 |
| Deposit | 2.2 | 1.9 |
| Merge | 1.0 | 0.8 |
| Shade pixels | 2.5 | 2.4 |
| Total | 8.6 | 11.5 |

Their settings: `l = 4`, `K = 4`, `|Ω_0| = 32` (`Θ_0 = 4`), `t_0 ≈ 1.6 Δs_0`,
`N = 4`. Merge is 12% of the frame; shading and deposition dominate. The paper
does not report memory. Any memory figure for Split RC is therefore unknown, not
"small".

Practical details that cost people time:

- Shading naively needs up to 256 unfiltered samples per pixel at 32 cascade-0
  directions. Split RC reduces this to at most 8 filtered samples by computing
  per-probe directional irradiance into a 6x6 octahedral texture first
  [P, arXiv 2607.20384 §6].
- A per-pixel gather over all directions is the single most expensive step in
  several implementations. Split RC measures 23 ms extra for a directional
  cascade -1 gather, against 1.4 ms for the cascade itself [P, §7.1].

### 4.3 Direction encoding

The paper's position is that the angular representation does not matter
asymptotically: cubemap, parabolic map, octahedral map and spherical harmonics
all scale the same way, and only the value count `Q_i ~ (2^i)^2` matters
[P, paper 2.5]. What does matter in practice is that the direction sets **nest**,
because the merge averages `K` child directions into one parent direction. Two
documented choices:

- **Octahedral tiles.** If cascade `i` has a `q_i x q_i` octahedral tile, cascade
  `i+1` has `2q_i x 2q_i`, and texel `(u,v)` of level `i` covers texels
  `(2u..2u+1, 2v..2v+1)` of level `i+1`. Nesting is exact by construction, and
  the merge is a 2x2 average. erhe uses this [P, erhe plan §1, §3].
- **Equal-area spherical mapping.** Split RC maps the sphere to `[0,1]^2` with an
  equal-area cylindrical map, generates `ω_{n,u,v}` on a `2Θ_n x Θ_n` grid, and
  uses `m_n(ω_{n+1,u,v}) = ω_{n, floor(u/2), floor(v/2)}` so four directions
  always map to one [P, arXiv 2607.20384 §4, Algorithm 2].

Split RC reports that octahedral and Clarberg octahedral mappings "result in
worse quality despite having a more uniform distribution" for their scheme, which
is a useful counter to the default assumption that octahedral is always better
[P]. Cody Bennett compares octahedral against equal-area octahedral in a 3D RC
implementation, again as an implementation note rather than a result [S].

Spherical harmonics fit the paper's requirement but no source here uses them for
cascades. Merging SH cones requires convolution or rotation, and the paper's own
merge is an average over a fixed child set, so SH would add work for no gain
[P for the requirement, A for the conclusion].

### 4.4 A complete voxel implementation, and where it departs from the paper

Shadertoy `M3ycWt`, "Volumetric Radiance Cascades" (Mathis, 2025-02-08), is the
only voxel-based implementation found here whose source can be read. It is a
better reference for layout and direction encoding than for light transport. Six
passes, about 32 KB of GLSL:

| Pass | Role |
|---|---|
| Buffer A | input state in a one-row texture: camera angles, position, mouse, animation flag |
| Buffer B | scene: a 32x32x48 voxel volume packed into a 256x192 atlas, albedo plus a flag (1 opaque, 2 emissive) |
| Buffer C | sun shadow map, ray marches the voxel grid, stores hit distance |
| Cube A | the cascades |
| Image | camera ray, shade, ACES tonemap |

Five levels. The probe lattice is `(32,32,48)/2^i`, so level 0 is one probe per
voxel face, and each probe holds a hemisphere, six per voxel. The direction tile
is `3·2^i` texels square, which is `9·4^i` directions at one per texel. RGBA holds
cosine-weighted and solid-angle-weighted irradiance in RGB and the ray's hit
distance in alpha:

| Level | Lattice | Dirs per face | Probe-faces | Texels | Atlas rows |
|---|---|---|---|---|---|
| 0 | 32x32x48 | 9 | 294,912 | 2,654,208 | 2592 |
| 1 | 16x16x24 | 36 | 36,864 | 1,327,104 | 1296 |
| 2 | 8x8x12 | 144 | 4,608 | 663,552 | 648 |
| 3 | 4x4x6 | 576 | 576 | 331,776 | 324 |
| 4 | 2x2x3 | 2304 | 72 | 165,888 | 162 |

Every row is exact: texels equal probe-faces times directions, and the row span
halves per level. The levels are separate row ranges in one 1024x6144 target, with
a hardcoded offset table `{0, 2592, 3888, 4536, 4860}`, not mip levels. Section
5.3 explains why that is forced.

Direction encoding: `ComputeDir` and `ProjectDir` map a square tile of texels to
a hemisphere by walking concentric square rings, with the azimuth count growing
as `4 + 8·floor(ring)`, and the ring's analytic solid angle gives the weight. All
texels are used and the mapping is monotone. It is simpler than octahedral plus
area correction, and worth copying for a first implementation.

Probe placement: one probe per voxel face, offset a quarter cell along the face
normal at level 0, shifted to a nearby empty cell at coarser levels by a `±0.5`
neighbourhood search. The author's own notes call this crude and name the fix:
find probe positions with volume mipmaps.

Merging: eight neighbouring probes at the next level up, four child directions
each, trilinearly weighted. Visibility is one fetch, from the coarse probe's
stored hit distance: if the coarse probe's ray ended before the vector between the
two probes, the weight is zero. Then a distance blend replaces the fine probe's
traced radiance with the coarse result as the hit distance grows. There is no
transmittance channel and no premultiplied composite.

Three departures from the paper:

- **No interval extension.** Every level traces the full scene AABB with a
  128-step DDA, so distance is bounded by the scene and the levels buy angular
  resolution, not reach.
- **No disjoint bands.** A distance blend replaces the band boundary, which is
  why the visibility cull and the temporal smoothing are needed at all.
- **The merge is temporal.** Level `i` reads level `i+1` from the previous frame,
  so light propagates one level per frame, and multibounce works the same way by
  adding the previous frame's cascade at each hit. Latency is `N` frames.

The verdict: this is the cascade geometry without the cascade algebra. The
layout, the direction encoding and the probe-occlusion trick are worth keeping.
The transport is multi-resolution probes with distance-based LOD blending.
`BRDF_GGX` is defined in the Common pass and never called, so the shipped passes
are diffuse only despite the description.

Cost at this size: 5.14M texels, about 82 MB at RGBA32F, 5.14M rays per frame.
Section 6 covers what happens when the world grows.

## 5. Data structures and storage

### 5.1 What is documented

- Sannikov's `Radiance 3d` uses "a 3d regular grid of radiance probes encoded in
  a volume texture" and "a 3d voxelization" as the scene representation. It is
  fully world space, so offscreen emitters light the scene. The paper's verdict:
  the hierarchy costs `2M_0`, but cascade 0 alone "is practically equivalent to
  voxelizing the entire scene", and "a hierarchy of 3d radiance cascades should
  be basically a drop-in improvement for any implementation that uses a uniform
  grid of radiance probes" [P, paper 4.3, 6].
- Interval extension is available exactly for rectilinear grids, which is the
  world-space and voxel case: trace `[a, a+x]`, then double to `[a, a+2x]` by
  shifting and merging. The paper reports this gives a dramatic reduction in
  raymarch cost for 3D grids [P, paper 2.3.3, 4.1, 4.3].
- Split RC replaces the dense grid with a sparse hashmap keyed on probe position,
  initialized from visible pixels and propagated upward one level at a time,
  with LOD by Chebyshev distance to the camera and no interaction between LODs.
  It interpolates only over probes that exist and renormalizes the trilinear
  weights [P, arXiv 2607.20384 §4, §4.1].
- erhe fits a dense grid to the content AABB, grows `s0` when the probe cap is
  exceeded, stores each cascade as its own atlas of `q_i x q_i` tiles, and wraps
  probe indices into rows to stay under the device texture size limit [P, erhe
  plan §3].
- Surfel RC places probes on surfels instead of a grid [P, `mxcop/src-dgi`].
  Mathis stores 3D probes in 2D textures so the problem becomes 2D-shaped
  [S, Shadertoy `X3XfRM`].

I found no published implementation that stores radiance cascades in an SVDAG,
brickmap, sparse voxel octree, or a probe clipmap. The search covered the RC
papers above, the implementations listed in section 4.2, and general web search
on the combination. This is a negative result, not proof of absence. The word
"voxel" in RC writing almost always refers to the scene representation used for
tracing, not to the probe storage. The one exception is `Radiance 3d`, where the
probes are a volume texture.

### 5.2 Mapping cascades onto voxel bricks (analysis)

The following is my reasoning from the documented properties. None of it is
measured, and none of it is implemented anywhere I could find.

**Two axes, not one.** Probe storage and scene traversal are independent. A
cascade can be stored in a brick pool and traced against an SVDAG, or stored in a
volume texture and traced against triangles, or any other combination. Most
confusion in this area comes from treating "voxel radiance cascades" as one thing
when it is two.

**Brick alignment is free if the brick edge is a power-of-two multiple of `Δp_0`.**
Take `Δp_i = Δp_0 2^i` and brick edge `B = Δp_0 2^k`. Every cascade then has an
integer number `(B/Δp_i)^3` of probes per brick, and the 2x2x2 merge is exactly a
reduce from brick level `i+1` to brick level `i`. The merge pyramid is the brick
pyramid, so the cascade index and the mip level of the brick pool are the same
number. This is the same structure a mipmapped clipmap already has.

**Occupied probes scale like a surface, not a volume.** Cascade `i` needs probes
only within `t_{i+1}` of a surface, and `t_{i+1}` grows as `2^i`, so the
qualifying region grows as a shell around the geometry. In a scene dominated by
surfaces, the number of occupied cells of a lattice of spacing `Δp_i` near those
surfaces scales as `area / Δp_i^2 = 4^{-i}`, while directions per probe scale as
`4^i`. The product is constant, which is the paper's surface-radiance scaling
(`M_i = M_0`) rather than the volumetric one (`M_i = M_0 2^{-i}`). This predicts
that surface-anchored sparse 3D cascades have constant memory per level, which is
consistent with Split RC's observation that probe count stays roughly constant
and that each level needs about the same number of rays. It also means the
`2M_0` bound is pessimistic for sparse storage and optimistic for dense storage,
in opposite directions.

**Empty and solid bricks are free.** For an empty brick, `β = 1` and `L = 0` in
every direction, and merging it with anything is the identity. For a brick fully
inside solid geometry, `β = 0` and merging terminates the chain. Both can be one
shared node, which is the same compression an SVDAG already performs on
occupancy. The compression ratio on the cascade data will track how much of the
scene is uniform, not how much of it is empty, so a scene with a surface in every
brick gains little.

**Sharing the scene SVDAG's nodes for radiance is not sound in general.** An SVDAG
node is shared when its children's occupancy patterns match. Radiance payloads
differ between two positions even when the occupancy under them matches, because
the radiance comes from different geometry. Aliasing radiance nodes is only valid
where the radiance is also identical, which in practice means the empty and solid
cases above. The workable design is a separate payload brick pool with the scene
SVDAG contributing traversal and, if you want, an occupancy classification that
drives payload allocation. Sharing the nodes themselves would be a correctness
bug, not an optimization.

**Interval extension interacts badly with non-lattice directions.** The doubling
identity needs `p + x·ω` to be readable as a sample of the same cascade. On a
lattice with spacing `x = Δp_i`, that only lands exactly on a probe when `ω` is a
lattice direction. For general directions it is an interpolated fetch. That is
legal because cascades are interpolateable by construction, but it is an
approximation, the error is a resampling error of the same order as the cascade's
own interpolation error, and it compounds once per doubling. Sannikov's paper
does not discuss this alignment condition; it states the identity for rectilinear
grids without qualification. Worth verifying against `Radiant 2d`'s code before
relying on it.

**Against a voxel DDA, the doubling trick buys less than it looks.** A voxel
marcher can already march exactly `[t_i, t_{i+1}]` with a `tmin`/`tmax` and no
extra machinery. The number of bricks a ray crosses in `[t_i, t_{i+1}]` grows with
`t_{i+1}/Δp_i`, which is constant per level by construction, so naive per-level
marching is already O(1) per level in brick count. The doubling helps mainly for
the top two or three levels, where the interval spans many bricks, and for
tracers without a native ray-interval query. erhe makes exactly this call from
the other direction: it skips interval extension because `rayQuery` takes
`tmin`/`tmax` natively [P, erhe plan §1].

**Clipmaps.** A camera-centred clipmap of bricks is the natural allocator for
world-space cascades in an unbounded world, and its level spacing doubling
coincides with the cascade spacing doubling. It is not a substitute for the
cascade hierarchy, though: every cascade is needed over the same region at once,
so the layout is (cascade x clipmap ring) pairs, and memory per ring is bounded
by `2M_0` as usual. erhe lists a "camera-centred clipmap volume" as follow-up
work [P, erhe plan §11].

**Concrete arithmetic.** erhe's numbers give the only concrete 3D memory anchor
in these sources: a 20 x 10 x 20 m volume at `s0 = 0.5 m`, `q0 = 4` is 32,000
cascade-0 probes, 512 K texels, about 16 MB for all cascades raw plus merged,
about 1 M interval rays for a full refresh [P, erhe plan §3]. That is 16 bytes per
texel across two copies (raw and merged), so 8 bytes per texel per copy, i.e. an
RGBA16F texel. Scaling that density by hand: a 256 m cube at 1 m spacing is
16.7 M probes, 268 M texels, about 4.3 GB at the same per-texel cost. That is
arithmetic on their reported density, not a measurement, and it is the reason
sparse or clipmap allocation is required rather than optional at scene scale.

**Layout constraints that bite in practice.** Atlas sides must stay under the
device texture size limit, and DDGI's naive `tiles_x = nx * nz` overflows at
cascade 0 densities [P, erhe plan §3]. A brick pool with a fixed brick size and an
indirection table avoids this entirely, which is a second, independent argument
for bricks over one big atlas.

### 5.3 Mip chains and 3D textures (analysis)

A mip chain looks like the right container because the resolution halving matches
the probe spacing doubling. It is not, for two independent reasons.

**A mip level is a low-pass of the one below; a cascade is a disjoint band.** A
mip chain is a sequence of successive approximations of one field. A cascade set
is a partition of the ray budget, and the sum over bands reconstructs the field.
That difference rules out all three mip mechanisms: the hardware cannot generate a
level (the finer level does not contain the far band), filtering across levels is
not the premultiplied composite, and level selection from ray differentials
answers "how much to blur" rather than "how far to see". A mipmapped radiance
volume is voxel cone tracing, which is what the paper replaces.

**The chain's capacity per probe is constant while the requirement grows.** Both
the probe lattice and the mip chain halve in all three axes per level, so texels
per probe is invariant: level `i` has `R^3·8^-i` texels and `P_0·8^-i` probes,
giving `R^3/P_0` at every level. Directions per probe are `q_0^2·4^i`. The chain
allocates memory in exactly the ratio the probe lattice shrinks, so it leaves no
room for the payload growth.

With one direction per texel (RGBA16F, RGB plus transmittance), a chain holding
`N` cascade levels must size level 0 so the top level still has a direction per
texel, `t >= 4^N·q_0^2`, and then:

```
chain total = R^3 · 8/7 = P_0 · t · 8/7
needed      = 2·M_0 = 2·P_0·q_0^2
ratio       = (4/7)·4^N
```

For `N = 4` that is about 146x the necessary memory, and `N = 3` is 37x. The
factor is independent of scene size and of `q_0`. The waste concentrates at level
0, and level 0 is half the hierarchy, so a chain wastes the majority of the data
to feed the minority.

Writing arbitrary data into each level does not avoid this. Mip levels are
subresources, so the semantics are yours, but the format is shared and the
capacity per probe is fixed by the chain's geometry. What survives is that a texel
at level `i` must hold `4^i` values, which no format allows past `i = 1` or `2`.

**Merge alignment and payload growth are mutually exclusive.** The one place the
ratios coincide is the 2x2x2 merge, which trilinear at LOD+1 computes for free.
That requires a parent probe at the texel-space centre of its eight children,
which holds only if the direction block occupies the same number of texels at both
levels. Constant texels per probe is exactly the case that forbids the payload
from growing. So a chain gives either a free spatial reduce or enough room for the
direction set.

**Where a chain does belong** is over the resolve target, not over the cascades.
Once the bands are collapsed, the diffuse radiance volume is a plain coarse grid
sampled with a wide cone, and mipmapping it is legitimate: wide cones read coarse
mips. That is the voxel cone tracing trick applied after the distance structure
has done its job, and the only place in the pipeline where blur is the point.

Mathis's implementation confirms the conclusion from the other direction: five
levels, one row range each, a different tile size per range, and a hardcoded
offset table (section 4.4). No implementation found stacks cascades in mip levels.

### 5.4 Sparse voxel octrees (analysis)

An SVO is the wrong container for the payload and the right structure for two
neighbouring jobs. The distinction matters because "voxel radiance cascades"
invites the assumption that a voxel hierarchy should hold them.

**Every access is a gather from a known cell, not a search.** The merge reads the
eight children of one fixed parent cell. Interval extension reads `p + x·ω`.
Allocation walks levels top down. Nothing asks where the data is, which is the
only question a tree answers faster than a flat index. A descent costs a
root-to-leaf walk per fetch and diverges across the warp, because neighbouring
lanes land in different subtrees. A Morton-keyed indirection table costs one
lookup with uniform cost for every lane.

**The payload does not fit in a node.** At `q_0 = 4` a probe goes from 16
directions (about 128 bytes) at level 0 to 1024 directions (about 8 KB) at level
3, while probe count drops by 32,768x. An SVO node is a child mask plus a pointer.
An 8 KB leaf is not a leaf, so the payload moves to a side pool and the tree is
only an index, which is the one thing a tree is worst at being.

**Node sharing is unsound.** An SVDAG compresses by aliasing subtrees with
identical occupancy. Two cells with identical occupancy underneath hold different
radiance, because the light reaching them comes from different geometry. Only
fully empty and fully solid bricks share honestly, and those are one bit of
classification per brick.

**The data is transient.** Cascades are rebuilt from the visible set every frame.
Building or refitting an octree per frame reconstructs a shape you already know:
fixed depth, fixed origin, support defined by a distance threshold.

**Sparsity pays where the data is already small.** With probes allocated near
surfaces, shell thickness in cells is `t_0/Δp_0` at every level, so per-level
memory is constant rather than shrinking. The payoff concentrates at level 0, and
the best case there is a factor equal to the linear resolution. At higher levels
the sparse set is larger than a dense lattice over the same extent by `2^i`,
though both are small.

Where an octree does belong:

- **The scene structure used for raymarching**, an independent axis from probe
  storage.
- **Surfel-anchored cascades.** If probes sit on surfels, the domain is a
  2-manifold and octree leaves are the surface. Still put the payload out of line.
- **An allocator, not a storage format.** A coarse occupancy tree deciding which
  brick pages exist is cheap, because it is only touched during allocation.
- **Region partitioning for unbounded worlds**, where the tree partitions regions
  and their cascade sets rather than voxels (section 6.3).

The workable container is a per-cascade brick pool with a Morton-keyed indirection
table: brick edge `8·Δp_i` so each brick holds `8^3` probes at every level, probe
count per brick uniform, byte size per brick still growing `4^i`. That is a set of
volumes with halving resolution, which section 5.3 says is required, with the tree
replaced by an index. Mathis's use of volume mipmaps for probe placement is the
octree-adjacent job done cheaply, and it is the one the author names as the fix
for his crude placement search.

## 6. Scaling to large worlds (analysis)

### 6.1 The cost law

Everything reduces to `M_0 = P_0·Q_0`, the level-0 probe count times directions
per probe, because the hierarchy costs `2·M_0` regardless of level count or reach
(section 1.5). World extent does not appear in that expression. What appears is
how many probes you populate, and at what spacing.

Mathis's layout calibrates this because it is fully specified: 104.6 texels per
voxel across all levels, 837 B per voxel at RGBA16F, one probe per voxel face.

| World, 1-unit voxels | Voxels | Memory at 16F |
|---|---|---|
| 32x32x48 | 49k | 41 MB |
| 128^3 | 2.1M | 1.8 GB |
| 256^3 | 16.8M | 14 GB |
| 512^3 | 134M | 112 GB |
| 1024^3 | 1.07G | 900 GB |

Volume anchoring is `L^3` and caps out around 200 to 250 units on a 12 GB card.
Surface anchoring is `L^2` and changes the regime: a 1024-unit world with a
surface at 1-unit spacing is 1M probes, 537 MB at 32 directions, and 33 MB at
4-unit spacing. A 4096-unit world at 4-unit spacing is 537 MB.

### 6.2 Reach

A stack's reach is `t_N ~ 2^N·Δp_0`, about 30 base spacings with the five levels
implementations use. It is not unbounded in practice: going further needs `4^N`
directions at the top probe, which is 32,768 directions at `N = 5` with
`Q_0 = 32`. Every implementation stops and hands the top band to sky or an
environment map.

That is why one stack cannot span a large world. Reaching 4000 units requires
`Δp_0` in the tens, which loses everything up close. The far field needs a
separate mechanism, not more levels.

### 6.3 Per-level allocation, not a window

The right formulation is an allocation policy per level rather than a compute
distance. Nothing in the algebra assumes a window: bands are disjoint, and the
merge reads by level index and spacing, not by region. A window is a policy saying
"allocate level 0 within radius R", and a second policy can say "allocate levels 3
and up wherever a surface exists, worldwide".

Numbers for that split, base spacing 1, `R = 256`, world 4096:

| Level | Allocation | Probes |
|---|---|---|
| 0 | volume, within R | 17M |
| 2 | surface, worldwide, 4-unit | 1M |
| 3 | surface, worldwide, 8-unit | 0.26M |
| 4 | surface, worldwide, 16-unit | 66k |

The worldwide tail is about 13 times cheaper than the near window, and it delivers
light from anywhere at high angular resolution, which is what distant light needs.
Total memory stays near `1.1x` the window alone.

Two things to get right:

- **Cut spatial frequency with distance, not angular.** Far probes need more
  directions, because a distant source subtends a small angle. Dropping the fine
  angular levels is exactly what makes distant geometry go blurry, the artifact
  the technique exists to prevent. Level 0 is the safe thing to drop.
- **Cut at band boundaries, never inside a band.** A per-ray distance cap is a
  correctness break: untraced space in a band reports transmittance 1 (light
  leaks) or 0 (a black wall). Fewer bands with a substituted top keeps the
  composite exact.

The top band has two candidates. An environment map is cheap but unoccluded, so a
mountain between the viewer and the sunset disappears. A coarse world-space probe
set keeps horizon occlusion and is just levels `N+1..` with large `Δp` and sparse
allocation.

### 6.4 What binds at scale

Tracing, not memory. Rays per frame is `2·P_0·Q_0`, one per cascade texel. Mathis
traces 5.14M rays with up to 128 DDA steps; at a 10 to 20 ms budget that puts the
ceiling around `10^7` rays per frame, or `P_0 ≈ 3·10^5` at 32 directions. Memory
would allow roughly `10^7` probes. Tracing is about 30 times tighter than storage.

The practical figure for a large world is therefore a few hundred thousand level-0
probes per frame: a 1 km square surface at 2-unit spacing, or a 400-unit window at
0.5-unit spacing. Getting both means clipmap rings, each ring constant-cost
because area grows 4x while density drops 4x, plus the per-level policy above.

Three failure modes to plan for:

- **LOD seams.** A probe crossing an allocation boundary changes value
  discontinuously when a coarser probe it merges from does not exist. Split RC
  renormalizes trilinear weights over existing probes; hysteresis on the
  allocation decision hides the rest.
- **Temporal history.** A probe that enters a window has no previous frame to
  merge from. Fade in over a few frames.
- **The boundary itself.** A black far term makes the window edge a visible ring
  that follows the camera; an unoccluded environment map makes it a lighting
  discontinuity. The top band must overlap the last real band rather than abut it.

## Sources

Primary, by the technique's authors:

- Alexander Sannikov, "Radiance Cascades: A Novel Approach to Calculating Global
  Illumination [WIP]", source and PDF:
  https://github.com/Raikiri/RadianceCascadesPaper
  (PDF: `out_latexmk2/RadianceCascades.pdf` in that repo). Dated 2023 in
  citations, never published.
- Rouli Freeman, Alexander Sannikov, Joe Pitt-Francis, "Split Radiance Cascades:
  Real-Time Global Illumination via Sparse Radiance Probes", arXiv:2607.20384v2,
  2026-10-11: https://arxiv.org/abs/2607.20384
- Rouli Freeman, Alexander Sannikov, Adrian Margel, "Holographic Radiance
  Cascades for 2D Global Illumination", arXiv:2505.02041:
  https://arxiv.org/abs/2505.02041
- Christopher M. J. Osborne, Alexander Sannikov, "Radiance Cascades: A Novel
  High-Resolution Formal Solution for Multidimensional Non-LTE Radiative
  Transfer", arXiv:2408.14425, published RASTI:
  https://arxiv.org/abs/2408.14425
- Alexander Sannikov, ExileCon 2023, "Rendering Path of Exile 2" (video):
  https://www.youtube.com/watch?v=TrHHTQqmAaM
- GGG forum announcement of that talk, 2023-11-01:
  https://www.pathofexile.com/forum/view-thread/3447463
- Alexander Sannikov, "Sparse Radiance Cascades: goodbye screenspace" (demo
  video, 4090 laptop): https://www.youtube.com/watch?v=rG2aok2SdbU
- `radiance-cascades.com`, the author-linked index of papers, demos and fixes:
  https://radiance-cascades.com/
- erhe engine, world-space radiance cascades plan and measurements (Timo
  Suoranta): https://github.com/tksuoran/erhe/blob/main/doc/plans/radiance_cascades.md
- `three-rc`, 3D RC with screen-space probes and a BVH (Cody Bennett):
  https://github.com/CodyJasonBennett/three-rc
- `src-dgi`, Surfel Radiance Cascades (Max Coppen):
  https://github.com/mxcop/src-dgi
- Mathis, "Volumetric Radiance Cascades", Shadertoy `M3ycWt`, 2025-02-08, source
  read in full: https://www.shadertoy.com/view/M3ycWt

Secondary:

- Max (mxcop), "Fundamentals of Radiance Cascades", with the penumbra condition
  and merge pseudocode: https://m4xc.dev/articles/fundamental-rc/
- Jason McGhee, "Building Real-Time Global Illumination: Radiance Cascades",
  interactive walkthrough and level-count rule: https://jason.today/rc
- Yaazarai (Alex) and Xor, "Guest: Radiance Cascades", GM Shaders:
  https://mini.gmshaders.com/p/radiance-cascades
- Radiance Cascades Wiki:
  https://radiance.wiki/ (paper page:
  https://radiance.wiki/papers/sannikov-original)
- Mathis, "Radiance Cascades 3D", Shadertoy `X3XfRM`:
  https://www.shadertoy.com/view/X3XfRM
- Tianlong Ruan (Infold Games), "The Evolution of Real-Time Global Illumination
  in Love and Deepspace", GDC 2026:
  https://www.youtube.com/watch?v=H49TUZY1pL0 (not read; cited here only as the
  deployment that Split RC cites)
- `noino.dev`, image-space radiance cascades project notes:
  https://noino.dev/projects/radiance-cascades/

## Open questions / what to verify

1. **PoE2 shipped parameters.** Nothing public gives cascade count, probe
   spacing, direction counts, mask layout, or memory. The ExileCon video is the
   only first-party artifact and its content is not transcribed anywhere I could
   find. If this matters, transcribe the video.
2. **Split RC memory.** The paper reports time in detail and memory not at all.
   Anyone quoting a memory figure for it is guessing.
3. **Dell'Ova 2025**, "Analyzing the Performance Scaling of Radiance Cascades"
   (Uppsala, DiVA `diva2:2007296`), is the one source that measures 3D RC scaling
   against resolution, probe spacing, level count and initial ray count. The host
   was unreachable during this pass, so its numbers are absent here. Retry.
4. **Interval extension and direction alignment.** Section 5.2 argues that the
   doubling identity needs lattice-aligned directions to land on a probe. The
   paper does not mention it. Check `Radiant 2d` and `LegitCascades` source.
5. **No SVDAG or brickmap RC implementation was found.** Confirm with the RC
   Discord before treating this as settled. The negative result may just mean
   nobody published. Section 5.4 argues a tree is the wrong container regardless,
   so the verification matters less than it looks.
6. **The 128-bit occlusion mask.** The paper mentions it in two sentences. The
   closest published relative is the visibility bitmask of Therrien et al. 2022,
   which Split RC cites for its cascade -1. Whether PoE2's mask is that scheme,
   or a different angular layout, is unknown.
7. **`Core Keeper` and `Kyora`** are cited by Split RC as shipped 2D RC games.
   Worth confirming with the developers if a 2D precedent matters.
8. **Radiant 2d's timing is internally inconsistent** (about 30 ms in the text,
   about 12 ms in the figure caption, both GTX 3060). Do not quote either as a
   benchmark without noting the discrepancy.
9. **Mathis's frame cost.** `M3ycWt` reports no timing. The pass structure implies
   5.14M rays per frame at 1024x6144, but the frame rate depends on the display
   GPU and is not published. Capture it before quoting any performance number.
10. **Per-level allocation in practice.** Section 6.3 is reasoned, not measured.
    Nobody in these sources publishes the cost of a dense window plus a sparse
    worldwide tail, and the seam behaviour at the window boundary is unmeasured.
    The first implementation should instrument both.
