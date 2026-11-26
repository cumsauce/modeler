# `decimate`

- **`decimate`** makes a lighter copy of a dense part (an `sdf` / `skin` /
  `deform` / `blob` result easily runs to 300k+ triangles): quadric edge
  collapse down to `ratio` of the triangles or a `tris` count, keeping
  closed solids closed and the shape's silhouette. It is an op like any
  other, so it is **non-destructive**: the dense input stays in the graph
  (hide it), any change to it re-derives the decimate, and re-issuing with
  another `ratio` re-tunes it. `bake: true` keeps the dense surface's look
  on the light mesh: the result's material bake (whatever it wears: a tile
  or object pbr material, or a flat colour) gets the dense surface's
  normals blended into its normal map and its offset added to `height`, on
  a per-part atlas of `resolution` texels. `cage` (meters) is how far from
  the light surface the bake looks for the dense one (default 1.5 x the
  largest gap measured, so it rarely needs setting). A 349k-triangle
  sculpt at `tris: 1500` with `bake` reads almost like the original.
  `{"type": "decimate", "name": "statue", "input": "statue_hi", "tris":
  4000, "bake": true}` plus `{"type": "hide_part", "part": "statue_hi"}`.
  An `sdf` / `skin` (without its own `resolution`) that only feeds
  `decimate`s with `tris` and no `bake` is meshed for them (about 40x the
  kept triangles, at least 6000) instead of at full detail: a 6 cm eye
  decimated to 80 triangles meshes 6k, not 127k. Independent slow ops
  (sdf, decimate, deform, booleans, ...) derive in parallel, and a batch
  evaluates its run of graph items once, not after every item.
  Moves, transforms, arrays, scatters and mirrors carry the bake along; an
  op that reshapes the mesh (deform, boolean, round, ...) drops it, so
  decimate last. Exports write the bake as the part's textures (per-part
  KTX2); export `lods` then simplify the light mesh further and reuse them.
