# `split`

- **`split`** cuts a closed solid by a plane into `name_above` (the side
  `plane.normal` points to) and `name_below`, both closed with flat caps:
  broken pots, cutaway views, parts that open or animate apart.
  `{"type": "split", "name": "vase", "input": "vase_raw", "plane":
  {"normal": [0, 1, 0], "point": [0, 0.15, 0]}, "gap": 0.02}`. `keep:
  "above"` / `"below"` makes only that half; `gap` pushes the halves apart
  along the normal (half each way). Hide the input. A plane that misses the
  part errors with the part's extent along the normal.
- **By a cutter**: `cutter` (a closed solid, hidden) instead of `plane`
  splits the input exactly into `name_inside` and `name_outside` in one
  pass: the two share one cut boundary and neither keeps a face lying on
  the other, so nothing flickers where they meet (an `intersection` plus a
  `difference` can leave coincident slivers on curved patches). Glass,
  lamps, doors, hoods and bumpers as their own parts, or soft-body panels
  with clean shut lines. `depth` (meters) takes only that skin under the
  input's surface inside (the inner boundary is the surface offset
  inward), so an inlay is a thin lens, not the whole overlap; `lift`
  moves the inside piece out along the mean normal of its outer surface
  (a pane 3 mm proud). `keep: "inside"` / `"outside"` makes one piece.
  `{"type": "split", "name": "door", "input": "body", "cutter":
  "door_cut", "depth": 0.015}` ->
  `door_inside` (the door skin) and `door_outside` (the body without it).
