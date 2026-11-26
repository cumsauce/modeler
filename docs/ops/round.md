# `round`

- **`round`** works on any closed solid:
  - `fillet` rounds convex edges by `radius` and keeps the outer size
    (Minkowski opening).
  - `chamfer` is a 45° bevel on axis-aligned edges.
  - `smooth` does subdivision rounding.

  Large or concave inputs make fillet/chamfer slower, so round parts before
  combining them. `fillet` takes `edges` to round only some edges: `top` /
  `bottom` (horizontal edges in the upper / lower half of the part's box),
  `horizontal` (both), `vertical` (edges along y), or a list (`["top",
  "vertical"]`: a table top rounded on top and at the corners, sharp
  underneath). Exact for axis-aligned edges (the opening uses a y cylinder
  for vertical edges, crossed x / z cylinders for horizontal ones, and the
  halves meet at mid-height); slanted edges round with their half's tool,
  and top / bottom need the part taller than 2 x radius.
