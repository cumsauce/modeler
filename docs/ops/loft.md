# `loft`

- **`loft`** skins a closed solid (ready for booleans) through two or more
  `sections`, which may change shape and size: a circle into a square, a
  bottle body narrowing to its neck, a hull, a horn, a blade, a tapered
  leg. Each section is a `profile` (one ring, points or arcs) at `y: h`
  (or `at: [x, y, z]`), optionally `rotation` (unrotated sections lie
  flat; `[a, b]` is world x / z like an extrude footprint) and `scale` (a
  number or `[a, b]`). Rings are resampled to the same point count
  (`samples`, default the most any section has, at least 32; corners of
  rings with fewer points stay sharp) with start points matched, so the
  skin doesn't twist. `smooth: true` runs a Catmull-Rom through the
  sections (`steps` rings per span, default 12) instead of straight
  ruled spans. With `path`, sections sit square to it at `t` (0..1 along
  it, default evenly spread; `[a, b]` along the path normal / binormal
  like sweep). `scale: 0` on the first or last section closes it to a
  point (a horn tip, a spire). A bottle:
  `{"type": "loft", "name": "bottle", "smooth": true, "sections": [{"y": 0,
  "profile": [{"arc": {"center": [0, 0], "radius": 0.035, "from": 0, "to":
  360}}]}, {"y": 0.16, ...r 0.037}, {"y": 0.22, ...r 0.014}, {"y": 0.28,
  ...r 0.013}]}`; hollow it with a smaller loft and a `difference`.
