# MATL alpha in the effective palette

MagicaVoxel material alpha is a second source of compositing coverage. The
renderer folds it into the Palette during world loading, then keeps the existing
single-layer voxel transparency path unchanged.

The effective alpha for a material is:

```text
effective_alpha = palette_alpha * matl_alpha
```

`MATL` IDs use MagicaVoxel's one-based numbering. ID `1` maps to palette slot
`0`, and IDs `1..=255` map to slots `0..=254`. ID `0` and IDs `256` and above
are ignored because a normal solid XYZI byte cannot select those slots. Legacy
zero-based material IDs are not inferred.

A material with no usable `_alpha` falls back to its palette alpha. A missing
material record, a nonnumeric value, a non-finite value, or a value outside
`0.0..=1.0` does not reject the world. Duplicate material IDs do reject the
world. A file with no `MATL` chunks is palette-only and produces no material
fallback warning. Warnings are aggregated.

Only `_alpha` is read. `_trans`, `_ior`, `_d`, and `_att` remain unsupported
material properties and do not change the result. When a referenced material
has no usable alpha, one aggregate warning reports those properties. No
material table or new GPU buffer is added. `get_palette` remains the raw RGBA
conversion, while `get_effective_palette` is the fallible conversion used by
both hosts.

A non-default or malformed `IMAP` chunk rejects the world. The standard default
map is accepted as a no-op. This keeps source-order palette and material lookup
paired without silently applying alpha to a remapped display slot.

## Considered options

- **Read `_trans` as coverage**. Rejected. It describes physical transparency
  and would imply attenuation or refraction, neither of which the voxel pass
  models.
- **Replace palette alpha with material alpha**. Rejected. Palette alpha remains
  part of the `.vox` contract; multiplying preserves both sources.
- **Add a material table to the renderer**. Rejected for this slice. The
  effective palette fits the existing `vec4[256]` upload and leaves the shader
  contract unchanged.
- **Ignore non-default `IMAP`**. Rejected. Source material indices and display
  palette positions would be paired incorrectly.
- **Infer legacy zero-based IDs**. Rejected. The format has no universal ID
  convention, and a file containing only IDs `1..=255` is ambiguous.

## Consequences

- Palette-only worlds keep their current output.
- MagicaVoxel glass such as `nuke.vox` can use `_alpha` without a renderer shader
  change when its source-order palette is used.
- A Material index is still a `u8` palette index in the World and voxel pool. The
  one-based `MATL` ID is metadata and is consumed only while constructing the
  uploaded palette.
- The existing alpha-0 skip, transparent shadow behavior, single-layer retrace,
  and Voxel-only rule continue to apply to effective alpha.
- Full `IMAP` remapping, physical glass, refraction, attenuation, and material
  lighting remain out of scope.
