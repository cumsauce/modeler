# `primitive`

- **Rounded primitives**: a `box`, `cylinder` or `wedge` primitive with
  `round: r` is built with every edge (a cylinder's rims) rounded by `r`,
  same outer size, `segments` arc steps per edge (4): a closed solid, ready
  for booleans, and far cheaper than a `round` op. `round: {"radius": r,
  "segments"?: n}` works too; edge selection needs a `round` item. Nothing real has knife
  edges, so give boxes a few mm. A style can make it the default:
  `"defaults": {"primitive": {"round": "$bevel"}}` (other kinds ignore
  `round`).

- **disc/washer/plane** are double-sided sheets in the XY plane (vertical);
  lay one flat with rotation `[-0.707, 0, 0, 0.707]`.
- **One-sided planes**: `{"type": "primitive", "kind": "plane", "size": [w, h],
  "facing": "up"}` (`up`/`down`/`+x`/`-x`/`+z`/`-z`) builds ONE quad, 4 verts
  and 2 triangles, front toward `facing`, back culled. Size is across the
  face: [x, z] for up/down, [z, y] for ±x, [x, y] for ±z. Still a small
  parametric op (nothing baked). Its UVs are 0..1 oriented like the matching
  box face, and tile materials re-project it with the same metric uvs as a box
  face, so tile textures line up with boxes. The part is marked `sheet`:
  `validate` doesn't call it open, and export never makes its material
  `doubleSided` (even a defaulted cutout). Use it for floors, ceilings and
  walls that are only seen from one side (tile kits) instead of thin boxes:
  no hidden faces, no coplanar overlaps at seams. `facing` on another kind is
  an error; not a boolean input (open).
