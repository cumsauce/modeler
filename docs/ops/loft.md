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
- **`quads: true`** makes the loft a cage for `smooth` `subdiv`: every
  section keeps its own points (all sections need the same count; no
  `samples`), `steps` is 1 unless given, and `subdiv` reads the walls as
  quads and each cap as one polygon. Point i of every ring is one line
  along the body, so put the points where the shape needs them (sill,
  shoulder, crown) and the same number on every station. A car body:
  stations along z (`at: [0, 0, z]`, `rotation: {x: -90}`: profile [x, y] is world x, y), then
  `{"type": "smooth", "mode": "subdiv", "levels": 2, "input": ...}`.
- **`fit: {side, top, levels?}`** fits a straight loft along z (sections
  at z stations, square to z) to blueprint outlines: `side` `[[z, y],
  ...]` (seen from +x) sets each ring's bottom and top at its z, `top`
  `[[x, z], ...]` (the plan; a + half, x >= 0, is mirrored) its width.
  Each ring keeps its own shape (the section's profile says where the
  shoulder and crown sit) and is scaled and moved in x and y, so the
  stations only need their shapes, not their sizes. Subdivision pulls the
  surface inside the cage (a few cm at a car's ends); with `quads: true`
  and `levels: 2` the cage is pushed out until the surface `smooth subdiv
  levels 2` makes spans the outlines at every station (iterated to
  0.2 mm). A ring outside an outline errors with its z. Check the result
  against the same outlines as images with `agent_views reference`.
