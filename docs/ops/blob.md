# `blob`

- **`blob`** melts closed solids together with a smooth fillet instead of
  a boolean's sharp seam: creatures, mushrooms, melted or sculpted forms.
  Each input becomes a signed distance field (sampled from its mesh, any
  closed solid works), they combine with a polynomial smooth-min of radius
  `blend` meters (default: a fifth of the smallest input's thinnest size; 0
  = a sharp union), and the result is meshed (Manifold's level set) into
  one closed solid with the first input's material. `op: "subtract"`
  carves the other inputs out of the first with a rounded rim (a dimple, an
  eye socket), `"intersect"` keeps the rounded common volume.
  `resolution` is the voxel size (default 1/120 of the result's size);
  over ~27M samples is refused with the size that fits. Hide the inputs.
  A blob of one input with a blend just remeshes it: use `round` for that.
  `{"type": "blob", "name": "shroom", "inputs": ["stem", "cap"], "blend": 0.03}`.
