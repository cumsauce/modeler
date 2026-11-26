# `stairs` and `railing`

- **`stairs`** is a flight as ONE closed solid (a staircase profile
  extruded across `width`, filled down to its foot, so it takes a tile
  material and passes `validate`): `{"type": "stairs", "name": "to_mezz",
  "from": [6, 0, 0], "toward": "+x", "rise": 4.4, "width": 4, "steps": 16,
  "run": 0.4, "collider": "blocks"}`. `from` is the middle of the lowest
  edge on the floor it climbs from; `toward` is "+x" / "-x" / "+z" / "-z" or
  degrees about +y (0 = +x, 90 = -z, 180 = -x, 270 = +z); `steps` defaults to
  a riser of at most 0.2 m and `run` (tread depth) to 0.3. A walker (and a
  player) climbs risers up to 0.3 m, so keep `rise / steps` under that.
  `collider: "blocks"` makes a `block` marker per step (`<name>_blk_<i>`,
  with the game's `Block` component, yawed with the flight). `railing`
  runs a top rail, posts (every `post_spacing`, at corners and both ends)
  and an optional mid rail along `path` (y = the floor it stands on) as one
  merged part, `collider: "blocks"` giving one full-height `block` per run.
  Leave a gap in a railing where people climb on or off. Both live in
  `src/stairs.rs`.
