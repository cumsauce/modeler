# `sway`

- **`sway`**: dynamic motion, baked into a clip of its own name. Every tool
  that plays clips plays it.
  - `kind: "wind"` (the default):
    `{"type": "sway", "name": "wind", "skeleton": "oak_rig", "direction":
    [1, 0, 0.3], "strength": 0.6, "gust": 0.4, "flutter": 0.5,
    "duration": 4}`.
    - Thick bones bend slowly and a little, thin ones faster and further,
      and the tips flutter.
    - `strength` is 0.5 for a breeze, 1 for a strong wind, 2 for a gale.
    - Every frequency snaps to whole cycles of `duration`, so the loop is
      seamless.
  - `kind: "spring"` is secondary motion: tails, ears, antennae, a hanging
    sign.
    - Each bone's tip lags behind where `over` (a looping clip) puts it, on
      a damped spring (`stiffness` 80, `damping` 8; `2 * sqrt(stiffness)`
      is no bounce).
    - `gravity` droops it, and a `strength` adds wind.
    - It bakes three loops of `over` and keeps the last, so it loops.
  - `bones: ["tail*"]` limits which bones sway; the rest only follow.
  - `duration` defaults to 4 s, or to the `over` clip's duration. With
    `over` it must be a whole number of that clip's cycles.
  - `fps` sets the export rate (30).
