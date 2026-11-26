# `path_array`

- **`path_array`** spaces copies of a part along a path (fence posts, rail
  ties, lamps along a road, chain links): `{"type": "path_array", "name":
  "posts", "input": "post", "path": [[0, 0, 0], [3, 0, 1.5], [6, 0, 0]],
  "smooth": true, "spacing": 1.2}` makes `posts_0..`, copy 0 at the path
  start and (open paths) one at the end, `spacing` rounded to fit;
  `count` instead fixes the number. Model the source at the origin with
  +x along the path and +y up: with `align` (default true) each copy turns
  so +x follows the tangent (y stays up on a level path), and the source's
  own translation is an offset in that frame (a post at y 0.5 stands on
  the path). `roll_step: 90` rolls each copy 90 degrees more about the
  tangent than the last (interlocking chain links); `closed` loops the
  path with no copy doubled at the seam. Hide the source.
  **On uneven ground** give `on: "ground"` (terrain, a hill, any part):
  the path is dropped onto its surface (path y becomes a height above it,
  usually 0; spacing is measured in plan) and re-follows when the ground
  changes. `follow` turns the copies: `upright` (default: posts, lamps,
  trees stand straight), `slope` (pitched along the ground: kerbs, walls,
  rails) or `normal` (on the surface normal). `between: true` puts copies
  between consecutive points instead of at them, each turned along and
  stretched over its span (model the source along +x, any length, centred
  on the origin): one fewer copy than points. A fence on a hill is two
  items with the same path and spacing:
  `{"type": "path_array", "name": "posts", "input": "post", "path": [[-10,
  0, -4], [0, 0, 2], [10, 0, -3]], "smooth": true, "spacing": 2, "on":
  "ground"}` and the same with `"name": "rails", "input": "rail",
  "between": true`: posts upright on the slope, rails running post to
  post.
