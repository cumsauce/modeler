# `material` items

A `material` item's `name` may be a pattern (`"wall_*"`): it colours every
matching part in place (parts already in the model plus every name the
batch makes), and the model keeps the pattern, so parts later batches add
(`wall_7` in the next call) get it too. A pattern that matches nothing when
issued is an error; array and scatter prefixes are never matched.
A `material` item may also carry `pbr` (a procedural material by name, see
`describe_op("topic:materials")`), with `params` overrides and `scale`.
A visible part with only a flat colour (no `pbr`) is a warning in
`scene_summary` and every `apply_ops` summary, and `verify materials:
"pbr"` fails it. When a plain colour is what the user wants, add
`"flat": true` to that material item and the part is left alone.

A `material` item also takes two **finish** passes, baked over whatever
the part wears (tile or object pbr, or a flat colour, which then gets the
plain `_detail` graph as decimate's bake does):

- `bevel: 0.005` (meters, or `"$bevel"`; a style `defaults: {"material":
  {"bevel": "$bevel"}}` gives it to every material item): normals bent
  toward the edge's average at every crease sharper than ~30°, fading over
  the radius, so a hard box edge catches a highlight with no geometry
  change. An `output` node's `bevel` does the same for every part wearing
  that material.
- `macro: {"amount": 0.15, "scale": 4, "ground_dirt": 0.3,
  "ground_height": 0.5, "seed": 1}`: low-frequency world-space noise
  drifting colour and roughness (`scale` meters), plus darkening and
  roughening toward y = 0 over `ground_height`, so a big tiled wall or
  floor doesn't read as repeats.

Either one makes the part's bake **per part** (its own 1024² atlas, cached
by mesh, finish and, for `macro`, its transform), not the shared tile
texture: exports write that part's own textures. Keep it for hero and large
surfaces, not hundreds of scattered copies.
