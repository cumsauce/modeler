# `smooth`

- **`smooth`** fairs or rebuilds a mesh (model space; hide the input):
  - `relax`: Taubin smoothing, `iterations` (10) rounds of `strength`
    (0.5): evens out lumps, pinches and noise without the shrink of plain
    Laplacian smoothing. Open borders stay put. Good after `deform`.
  - `remesh`: a closed solid rebuilt with even triangles `edge` meters
    long (default the input's mean edge), then relaxed along the surface.
    Edges sharper than `edge` round off. Use it before `deform` or after
    booleans that left slivers.
  - `subdiv`: Catmull-Clark of a coarse cage, `levels` 1..5 (2): a box
    becomes a smooth pebble, a few boxes a soft character. Coplanar
    triangle pairs are merged back into quads first, and edges whose faces
    meet at more than `crease` degrees stay sharp (`crease: 60` on a
    cylinder cage keeps the rims, smooths the sides); open borders stay
    sharp too.
