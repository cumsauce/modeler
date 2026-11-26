# `terrain`

- **`terrain`** makes the ground of a level: a closed slab (heightfield
  top with smooth normals, vertical skirt walls, flat bottom at `base`)
  centred on the origin, so booleans, `place`, `grounded` and `probe_ray`
  all work on it, and metric uvs map tile materials at real size. The top
  runs from about y = 0 (valleys) to `height`; `scale` is the feature size
  (hill spacing, dune wavelength) and `resolution` the cell size (at most
  1M cells; 60 x 60 at 0.5 m builds in milliseconds). Styles: `hills`,
  `ridges` (sharp crests), `dunes` (long windward slopes, steep slip faces
  across `wind` degrees), `mesa` (`terraces` flat treads and cliffs),
  `flat` (a level slab for pads and paths only). Same `seed` = same
  ground, and a bigger `size` extends it without moving what was there.
  `flatten: [{"at": [x, z], "radius": 6 | "size": [w, d], "height"?: y,
  "falloff": 3}]` levels a pad for a building (height defaults to the
  ground's own at `at`); `paths: [{"points": [[x, z], ...], "width": 3,
  "depth": 0.3, "falloff"?}]` carves a bed level across its width that
  follows the ground lengthwise (a road at 0.1-0.3, a river at 1-2; pads
  win where they cross). `edge_falloff` eases the border down to y = 0.
  Then `place` buildings onto it (`{"type": "place", "parts": ["hut"],
  "on": "ground", "at": [x, z]}`) and colour it with a tile material that
  mixes grass and rock by `facing` up.
