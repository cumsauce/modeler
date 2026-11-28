# `cage`

- **`cage`** is a polygon cage for `smooth` `subdiv`: `verts` [[x, y, z],
  ...] and `faces`, lists of vertex indices (quads mostly; triangles and
  larger polygons work), each wound counter-clockwise seen from outside, so
  a shared edge runs opposite ways in its two faces. The part is the cage
  triangulated (hide it); `subdiv` reads the polygons themselves, so a
  curved cage (a car body's sections, a hull) subdivides as quads, without
  the lumps a triangulated cage leaves in the reflections. Moves and
  materials keep the polygons; anything that changes the mesh (a boolean,
  a mirror op, a deform) drops them, so subdivide the cage itself.
- `creases: [[a, b, weight], ...]` per edge: the edge a-b stays sharp for
  `weight` levels and rounds after (semi-sharp: `1` keeps a crisp but
  rounded edge at levels 2-3, `0.5` only firms it), `10` or more is sharp
  at every level. A vertex on two creased edges follows them, on three it
  is a corner. `subdiv`'s `crease` angle still applies on top.
- `mirror: "x"` (or y / z): give the + half (every vertex at x >= 0); it is
  mirrored across x = 0 and joined there (vertices on the plane are
  shared, so leave the half open along it).
- A car body from cross-sections is a `loft` with `quads: true` (the
  rings' own points become the cage); `cage` is for shapes a loft can't
  give: a cab with a separate roof face, a wheel arch notch, faces of
  other sizes.

```json
{"type": "cage", "name": "pod_cage", "mirror": "x",
 "verts": [[0, 0, -1], [0.5, 0, -1], [0.5, 0.6, -1], [0, 0.7, -1],
           [0, 0, 1], [0.4, 0, 1], [0.4, 0.5, 1], [0, 0.55, 1]],
 "faces": [[0, 3, 2, 1], [4, 5, 6, 7], [1, 2, 6, 5], [2, 3, 7, 6], [0, 1, 5, 4]],
 "creases": [[1, 5, 10]]}
{"type": "smooth", "name": "pod", "input": "pod_cage", "mode": "subdiv", "levels": 3}
```
