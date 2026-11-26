# `silhouette`

- **`silhouette`** carves a solid from two or three orthographic
  outlines, the way a modeler blocks out a car, shoe, gun or plane from
  blueprints: `side` (`[[z, y], ...]`, seen from +x, front = +z), `top`
  (`[[x, z], ...]`, the plan) and optional `front` (`[[x, y], ...]`). Each
  outline is a closed profile (points or arcs, several rings = holes,
  `round` fillets its corners in 2D, `smooth: true` runs a closed
  Catmull-Rom through the points so a few points make a curved body),
  extruded through the third axis as a 2D distance field; the solid is
  their intersection, smooth by `blend` meters (rounds the edges where
  outlines meet; default 0 = sharp), meshed like `sdf` (`resolution`,
  `tris`, `bake`), then moved by `translation` / `rotation`. A car body:
  `{"type": "silhouette", "name": "car_body", "smooth": true, "blend":
  0.12, "side": [[2.2, 0.3], [2.25, 0.75], [1.0, 0.85], [0.4, 1.4], [-1.2,
  1.4], [-2.2, 0.95], [-2.2, 0.3]], "top": [[-0.9, 2.2], [0.9, 2.2], [0.95,
  -2.2], [-0.95, -2.2]], "front": [[-0.95, 0.3], [0.95, 0.3], [0.95, 0.9],
  [0.6, 1.4], [-0.6, 1.4], [-0.95, 0.9]], "tris": 6000}`; cut wheel arches
  with a boolean or an sdf. It is also an sdf shape, `{"shape":
  "silhouette", "side", "top", "front"?, "round" (= the edge blend),
  "smooth"?, "at"?, "rotation"?}`, so it blends with spheres and chains
  (a fuselage plus wings). `from_reference: {"image":
  "refs/car_side.png", "view": "side", "height": 1.4}` (or a list, one per
  view; fields `{image, view, height | size, flip?, regions?, holes?}`)
  in place of a profile traces the subject of a PNG in the project
  (opaque pixels, or those contrasting with the border: the same
  extraction as the `agent_views` reference compare), simplified to about
  a pixel. Views: `front` (seen from +z, image right = +x) and `back`
  (from -z, right = -x) fill `front`; `side` / `right` (from +x, right =
  -z, so a drawing facing left faces +z) and `left` (from -x, right = +z)
  fill `side`; `top` is the plan (right +x, down +z). `flip: true`
  mirrors the image left-right. Holes are kept (`holes: false` drops
  them); `regions: true` (or a share, 0..1) keeps separate regions at
  least that share of the largest, else only the largest. `height` scales
  the outline's vertical extent to meters (uniform); `size: [w, h]` sets
  both. Front / side outlines stand on y = 0 centred, a top outline is
  centred. The op stores the image path, view and a hash of its content:
  a changed PNG re-traces on the next re-derive (an sdf `silhouette`
  shape re-traces only when the item is re-applied).
