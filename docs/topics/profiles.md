# Profiles (extrude, revolve, sweep, loft, silhouette)

- **Extrude / revolve** build closed solids from 2D profiles (Manifold),
  ready for booleans. `profile` is one ring `[[a, b], ...]` or a list of
  rings; inner rings are holes, orientation doesn't matter (even-odd).
  - `extrude`: footprint points are `[x, z]` on the ground, extruded up
    y = 0..height. `twist` (degrees) and `scale_top` act about the profile
    origin `[0, 0]`, so centre the footprint there for a symmetric taper
    (`scale_top: [0, 0]` = pyramid/spire). A flat cut-out shape (flag, sign,
    gear) is a thin extrude stood up with rotation `[-0.707, 0, 0, 0.707]`
    (footprint z becomes height, thickness goes to -z).
  - `revolve`: points are `[radius, y]` (radius >= 0) around the y axis:
    vases, bowls, columns, bottles. `degrees` < 360 leaves flat end caps.
    Hollow a vessel by revolving an inner profile and taking a `difference`.
- **Profiles** (extrude/revolve/sweep/loft): ring entries are points or arcs,
  `{"arc": {"center": [a, b], "radius": r, "from": deg, "to": deg}}`
  (counter-clockwise from +a; 0 -> 360 is a full circle), so curves need no
  hand-made point lists. `round: r` fillets every profile corner; on a full
  360° revolve the fillet arcs at corners turning under 40° keep the
  unrounded corner's soft shading (no flat bands), sharper corners stay
  creased.
  **SVG profiles**: anywhere a profile goes (extrude, revolve, sweep, loft
  sections, silhouette side / top / front) give `{"svg": "<svg ...>" or
  "<path d=...>", "size"?: [w, h] | w, "height"?: m, "center"?: [a, b],
  "flip_y"?: true, "id"?: "element"}`: every filled shape (strokes as
  outlines, text as glyphs) becomes rings, holes kept (even-odd), the
  content's bounds fitted to `size` / `height` (default 1 unit = 1 mm) and
  centred on `center` (default origin); `flip_y` (default) makes the
  drawing's up +b. Sweep and loft take its largest ring. An emblem:
  `{"type": "extrude", "name": "emblem", "height": 0.01, "profile":
  {"svg": "<path fill-rule=\"evenodd\" d=\"M0 0 H100 V100 H0 Z M25 25 H75
  V75 H25 Z\"/>", "size": [0.2, 0.2]}}`.

- **Smooth paths**: `sweep`, `path_array` and a path `loft` take
  `smooth: true` (or `curve: "catmull_rom"`): `path` is then a few control
  points of a centripetal Catmull-Rom curve that passes through every one,
  sampled about every 5 degrees of bend (`samples` per span overrides), so
  a hose is 4 points, not 40. `sweep` also takes `twist` (degrees from
  start to end) and `scale_end` (profile scale at the end, by length: a
  tapering horn or tail).
