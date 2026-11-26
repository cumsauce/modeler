# `scatter`

- **`scatter`** strews copies of parts over another part's surface: rocks
  on terrain, grass tufts, mushrooms on a log, rivets on a hull, a forest.
  `{"type": "scatter", "name": "rocks", "input": ["rock_a", "rock_b"],
  "on": "ground", "count": 40, "seed": 2, "scale": [0.6, 1.4], "rotate":
  "random", "max_slope": 35}`. Points are sampled area-weighted on `on`'s
  upward-facing faces (`faces: "all"` for sides and undersides), rejected
  by `max_slope` (degrees from level), `region` (`{"min": [x, z], "max":
  [x, z]}`) and `min_spacing` (meters; default 0.8 x the widest copy's
  footprint, so copies don't overlap; 0 allows clumps), so `count` is a
  target: the result says when fewer fit. `density` (copies per m², capped
  by `max`, default 2000, at most 10000) instead of `count`. Copies are
  `name_0..`, each standing with its base (the centroid of its lowest
  slice: a trunk, not the crown) on the surface, sunk until that whole
  slice is at or below the surface's tangent plane (no downhill gap under
  an upright trunk; at most 15% of the copy's height): `verify grounded`
  passes; `sink` pushes them further in. Steep spots tilt copies into the
  slope (a big rock on a cliff fails `collisions`), so give rocks and trees
  a `max_slope`. A `"rocks_*"` material colours the copies that were
  placed (slots that didn't fit are skipped, also after a re-seed); colouring
  the source works too, copies inherit it. Separate scatters don't avoid each other (grass may grow
  inside a rock). `rotate`: `random_y` (default),
  `random` (tumbled rocks) or `none`; `align` 0 stays upright (trees), 1
  stands on the normal (mushrooms round a log with `faces: "all"`).
  `input` entries: a part; a list `["oak", "oak_leaves"]` moved as one set;
  a pattern `"tree1.*"` (every visible part it matches, a prefab instance);
  a lone lsystem name brings its leaves (`"oak"` = `["oak", "oak_leaves"]`).
  One entry is picked per copy (`weights` for odds). Set members are named
  `name_i` (the first), `name_i_leaves` (a member named after the first),
  `name_i.trunk` (prefab members). Copies keep their source's material and
  are always visible: **hide the sources**. Same seed = same layout;
  `on` is a real dependency, so lower the ground with a param and every
  copy follows (it reads `on` before post-graph moves/places). Exports share
  one mesh between copies (instancing keeps each node's rotation and scale);
  use tile pbr materials on scattered parts (object ones bake per copy).
  **Smarter placement** (all optional, combine freely): `near: "wall"` (or
  a list) keeps only points within `near_distance` (default 0.5 m, a 3D
  distance to the nearest point of those parts' surfaces, not a plan
  distance) of those parts' surfaces (clutter along wall bases, moss round a rock);
  `edge_bias: 0..1` prefers points near `on`'s borders, creases and
  corners (1 hugs them; the falloff is a tenth of the surface's span), so
  debris collects at the foot of walls and along kerbs; `clump: {"count":
  5, "radius": 1.5}` picks 5 centres and gathers the copies within 1.5 m
  of them, denser at the middle (flower patches, mushroom rings); on a
  `terrain`, `mask: "path"` / `"pad"` keeps only points on its carved paths
  / flattened pads and `"not_path"` / `"not_pad"` keeps them off (the same
  bits as the heights file; `on` must be the terrain item itself, a mask on
  anything else is an error); `height: [min, max]` keeps world y in range
  (reeds only in the low ground). `count` stays a target: the result says
  how many fit.
  **Decals**: give `decal` instead of `input` to strew decals (cracks,
  stains, fallen leaves, puddles): `{"type": "scatter", "name": "stains",
  "on": "floor", "count": 20, "edge_bias": 0.6, "decal": {"pbr": "stain",
  "size": [0.2, 0.5], "aspect": 1, "roll": "random"}}` makes decals
  `stains_0..`, each centred on a sampled point and projecting into the
  surface along its normal (so they work on walls and slopes with `faces:
  "all"`), `size` the image width (a number or a random [min, max]),
  `aspect` height / width, `depth` the box depth (default half the smaller
  side). Every placement field works; `scale` / `rotate` / `align` /
  `sink` / `weights` don't (they move parts). They are ordinary decals
  everywhere decals go (preview, agent views, export, maps) and re-sample
  when `on` changes.
