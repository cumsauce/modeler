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
