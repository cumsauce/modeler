# `deform`

- **`deform`** reshapes a part's mesh for organic forms. The input is baked
  to model space and refined to edges no longer than `detail` first (default
  `scale / 8` for displace, 1/32 of the axis length otherwise, capped near
  250k triangles), so a low-poly sphere becomes a lumpy rock, not a spiky one
  and a 12-triangle box can bend. A closed solid stays a closed,
  outward-wound solid (booleans still work), UVs ride through, and open
  sheets are subdivided and deformed too. Hide the input.
  - `displace`: moves every vertex along its normal by 3D noise: `amount`
    (meters, the largest offset; default 12% of the smallest size; negative
    dents), `scale` (feature size, default 45% of the largest size),
    `octaves` (4), `seed`, `noise` `fbm` (lumps) / `ridged` (crags, cliffs,
    bark) / `turbulence`. A rock: a sphere scaled flat, then
    `{"type": "deform", "name": "rock", "input": "rock_raw", "mode":
    "displace", "amount": 0.08, "seed": 7}`.
  - `twist`: `degrees` about `axis` (default y) from its low end to its high
    end (a twisted bar, a drill bit, a rope strand).
  - `taper`: the cross-section scales from 1 to `scale_end` (a number, or
    `[a, b]` for the two cross axes in x/y/z order) along `axis`; `power`
    bends the profile (2 = stays thick, thins late).
  - `bend`: the part's `axis` extent curls into an arc of `degrees` toward
    `toward` (`x`, `-z`, ... or right/back/...; not the axis itself), the
    low end staying put. A horn is a cylinder tapered, then bent:
    `{"mode": "taper", "scale_end": 0.08}` into `{"mode": "bend", "degrees":
    120, "toward": "-z"}`; a curved blade bends a thin box.
  - `material`: real relief from a procedural material's `height` output
    (bark, scales, wrinkles, cobbles, bricks): `{"type": "deform", "name":
    "trunk", "input": "trunk_raw", "mode": "material", "pbr": "bark"}`.
    Each vertex moves along its (smoothed) normal by the height minus its
    mean, times `amount` (default 1: the channel's own meters); `scale` and
    `params` work as on a material item. Tile materials are baked once and
    sampled triplanar at real size, the same metric projection their
    colour uses, so giving the result the same `pbr` lines colour and relief
    up; object materials are evaluated per vertex (neighbourhood nodes such
    as blur see a strip there: build their height from noise / SDF nodes).
    `detail` defaults to two texels of the bake. Relief fades out within a
    few relief-depths of sharp creases (a cylinder's rim, a box edge) so
    the edge doesn't fold. Editing the material with `material_ops`
    re-derives the relief.
  Every number takes params (`"degrees": "$curl"`), and chains of deforms
  re-derive like any op.
