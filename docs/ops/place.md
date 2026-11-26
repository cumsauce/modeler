# `place`

- **`place`** drops parts onto a surface so nothing is placed by hand
  arithmetic: `{"type": "place", "name": "mug_on", "parts": ["mug"], "on":
  "desk_top", "at": [-0.4, 0.1]}` slides the mug's footprint centre to
  x -0.4, z 0.1, then moves it vertically until its underside rests on the
  desk's top surface (sloped and uneven tops work; `gap` leaves clearance).
  In `move` and `place`, a `parts` glob (any pattern with `*` other than a
  bare `*`) matches visible parts only, so `"petal*"` never grabs hidden
  `_raw` intermediates (the result notes how many it skipped); exact names
  still match hidden parts, and `["*"]` carries everything, hidden cutters
  included.
  It is a named, post-graph move like `move`, so it re-runs on every change:
  lower the desk with a param and the mug follows. `toward` sets the travel
  direction: `down` (default), `up` (under a shelf), or `back` / `front` /
  `left` / `right` to mount against a wall; `at` then names the two cross
  axes: back/front `[x, y]`, left/right `[z, y]`. `at` may also be an
  anchor (`"cat:paw_l"`, `"desk:top-front"`): its cross-axis coordinates
  are used, read before this place moves anything. A frame on the left wall:
  `{"type": "place", "name": "hang", "parts": ["frame"], "on": "wall_left",
  "toward": "left", "at": [-0.4, 1.5]}`. A place runs after any move/place
  that moves the part it lands on (a pot on a sill mounted on a wall),
  whatever order they are written in. List every part that should move
  (`"bracket*"` takes the array copies too). `each: true` places every
  matched part on its own instead of as one body: each keeps its footprint
  and travels until it meets `on`, so 43 keys land on a sloped deck in one
  item (`{"type": "place", "name": "keys_on", "parts": ["key*"], "on":
  "deck", "each": true}`; not with `at`).
