# Part edits: `hide_part`, `show_part`, `rename`, `delete`, `tri_mesh`, `uvs`

- **`hide_part` / `show_part`** `{part}`: `*` matches anything (`"cut*"`
  hides every cutter, `"*_raw"` every raw blank). They run after every op in
  the batch, in written order, so `"*_raw"` sees every part the batch makes
  and a later `show_part` wins. Hide the inputs of booleans, deforms,
  scatters and arrays; a visible input overlapping its result fails
  `collisions` with a hint saying so.
- **`rename`** `{part, to}`: also runs after the ops.
- **`delete`** `{name}`: removes an op output (or a baked part) and its own
  in-place hide / material / transform edits. It refuses while another op
  still reads it: delete or repoint those first.
- **`tri_mesh`** `{name, verts, indices, normals?, uvs?}`: baked raw
  triangles. Edits on baked parts change the part directly instead of
  adding ops.
- **`uvs`** `{part, projection}`: box / planar (along the part's thinnest
  axis) / cylindrical / spherical / metric (box in meters) / `face` (alias
  `label`: one upright, unmirrored image stretched over each side, for signs
  and labels). A projection on an op-derived part is remembered
  (`Model.uv_projections`) and re-applied after every re-derivation. A
  procedural `pbr` material re-maps its parts after the graph, which
  replaces any `uvs` projection on them. A card material with an opacity
  cutout on a closed plate with `face` uvs draws single-sided.
