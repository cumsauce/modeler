# `split`

- **`split`** cuts a closed solid by a plane into `name_above` (the side
  `plane.normal` points to) and `name_below`, both closed with flat caps:
  broken pots, cutaway views, parts that open or animate apart.
  `{"type": "split", "name": "vase", "input": "vase_raw", "plane":
  {"normal": [0, 1, 0], "point": [0, 0.15, 0]}, "gap": 0.02}`. `keep:
  "above"` / `"below"` makes only that half; `gap` pushes the halves apart
  along the normal (half each way). Hide the input. A plane that misses the
  part errors with the part's extent along the normal.
