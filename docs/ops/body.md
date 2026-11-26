# `body`

- **`body`** starts a creature or vehicle from a parametric preset, a
  skin of named joints with oval sections, standing on y = 0, facing +z
  (left = +x):
  - presets: `biped` (a person, `height` = standing height, default
    1.75, head 1/7.5 of it), `human` (a template mesh: `describe_op("topic:human")`), `quadruped` (a cat: `height` = shoulder
    height, 0.25), `bird` (standing height 0.25), `fish` (length 0.4),
    `serpent` (length 2), `car` (length 4.3; build = width, leg_length =
    wheel size, snout / tail = front / rear overhang, head_ratio = cabin
    height).
  - proportions: `head_ratio` (biped: head height / height; others
    relative to their default: quadruped 0.36, bird 0.2, fish 0.25,
    serpent 0.03, car 0.12), `build` (girth: 0.7 slim .. 1.5 heavy),
    `leg_length`, `tail` (0 = none; biped default 0, others 1), `snout`
    (muzzle, beak, nose).
  - `joints: {"paw_l": {"scale": 1.2, "offset": [0, 0, 0.01]}}`: scale
    sizes the sections at that joint, offset (meters) moves it with every
    joint below it; the body is grounded again afterwards. An unknown
    name lists the preset's joints (biped: hip, spine, chest, neck, head,
    jaw, clavicle/shoulder/elbow/wrist/hand, thigh/knee/ankle/ball/toe
    `_l`/`_r`; the clavicles sit between chest and shoulder so a rig can
    shrug;
    quadruped: hip, spine, chest, neck, head, snout, ear, shoulder,
    elbow, wrist, paw, thigh, knee, hock, foot, tail_1..4; ...).
  - it brings a skeleton `<name>_rig` with exactly those joints (unless
    `rig: false` or a skeleton item has that name) and, with `bind`
    (default true with the rig), an implicit smooth bind `<name>_bind` of
    the part to it, so clips work at once; a `bind` item on that rig
    replaces it, `bind: false` turns it off.
  - the car is a boxy superellipse body with a rounder cabin. The biped
    follows the ANSUR II male medians (2012 US Army anthropometric
    survey; widths, depths and landmark heights as fractions of stature,
    a leaner waist; `build` 1.15 reaches the median waist), checked by
    the `the_default_biped_matches_anthropometric_medians` test: its
    torso and muscles are ellipsoids blended over single tapered limb
    segments, with a mannequin face (brow, nose, shallow sockets, lips,
    jaw, ears) and arm span 1.03 x height. The default voxel is 1/135 of
    the size (the biped's 1/160).
  - the joint names are anchors on the part: `"cat:paw_l"` works as a
    move / rotation pivot, a primitive `frame` and a skeleton head or
    tail, and follows moves of the part.
  - `tris` keeps it light: `{"type": "body", "name": "cat", "preset":
    "quadruped", "tris": 6000}`.
