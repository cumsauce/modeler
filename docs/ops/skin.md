# `skin`

- **`skin`** wraps a skeleton in one smooth surface (like a skin modifier
  or Z-spheres): tentacles, roots, worms, hands, simple characters.
  `chains` are lists of points `[x, y, z, radius]` or names of `joints`
  (`{"hip": [0, 0.5, 0, 0.08]}`); each chain is a smooth tube through its
  points with the radius following them (`smooth: false` for straight
  rounded cones), and chains melt together where they meet by `blend`
  (default half the median radius). Same `mirror`, `warps` and
  `resolution` as `sdf`. An octopus: `{"type": "skin", "name": "octo",
  "mirror": "xz", "joints": {"body": [0, 0.25, 0, 0.12]}, "chains":
  [["body", [0.14, 0.1, 0.05, 0.045], [0.3, 0.03, 0.12, 0.028], [0.42,
  0.06, 0.26, 0.012]], ["body", [0, 0.45, -0.03, 0.11]]]}` (with `xz` one
  arm makes four).
