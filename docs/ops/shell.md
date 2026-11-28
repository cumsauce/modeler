# `shell`

- **`shell`** hollows a closed solid: every face moves in by exactly
  `thickness` (erosion by a ball, so convex corners stay sharp), and each
  face named in `open` (top/bottom/left/right/front/back, world axes; or
  compass words north = back (-z), south = front (+z), east = right, west =
  left) is left open. A cabinet carcass: `{"type": "shell", "name": "cab", "input":
  "cab_box", "thickness": 0.018, "open": ["front", "top"]}`; a planter or
  well: `open: "top"`; a ring or band: `open: ["top", "bottom"]`. Hide the
  input. Doors over an open front should overlap the side panels, or they
  touch only air and fail `grounded`.
- **Dense meshes** (a subdivided car body, a sculpt): the exact erosion
  takes inputs up to 20,000 triangles; above that `shell` uses `method:
  "offset"` (or ask for it): the solid minus itself moved in by
  `thickness` along its vertex normals, even at bends. Sharp convex edges
  come out slightly rounded inside, `open` is not available (cut the
  opening with a boolean), and a wall thicker than the tightest bend's
  radius is refused (the inner surface would fold). A 15 mm skin to cut
  shut lines from: `{"type": "shell", "name": "skin", "input": "body",
  "thickness": 0.015}`. An outward offset (a pane proud of its body) is
  `deform` mode `offset`.
