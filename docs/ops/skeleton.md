# `skeleton`

- **`skeleton`**:
  ```json
  {"type": "skeleton", "name": "rig", "mirror": "x", "bones": {
    "hip": {"head": [0, 0.9, 0], "tail": [0, 1.0, 0]},
    "thigh_l": {"parent": "hip", "head": "leg_l:top", "tail": "leg_l:bottom", "radius": 0.06}}}
  ```
  - Heads and tails are points or part anchors, so bones follow the geometry
    when params change.
  - `mirror: "x"` makes a `_r` twin of every `_l` bone.
  - `radius` is the envelope for smooth weights (default a fifth of the
    bone's length): about the limb's thickness.
  - `limits: {"x": [-150, 0], "z": [-10, 10]}` (degrees) constrains a
    joint. The bone's clip rotation (its offset from rest, in its rest
    frame) is split into Euler angles in the order rotations `{x, y, z}`
    apply (x, then y, then z) and each named axis is clamped: after the
    tracks, again after IK and after `look_at`. Mirror twins get y and z
    flipped. Exports write them to the joint node's `extras.limits`.
  - `look_at: {"target": [0, 1.6, 2] | "ball:center" | "head_target",
    "axis"?: "z" | "-x" | [x, y, z], "weight"?: 0.8}` turns the bone
    after IK so its `axis` (in its rest frame, default head -> tail)
    points at the target: a point, a part anchor or another bone's posed
    head (eyes, a head tracking something).
- **Auto-rigs (`from`)**: instead of listing bones, `from` names an op.
  - **An `lsystem`**: bones run along its branches (`trunk_0..`,
    `branch7_0..`), one every `bone_length` meters (0.5), down to `max_depth`
    branch levels (3), within `max_bones` (128, at most 256), thickest
    branches first. Finer twigs ride the bone they grow from.
  - **A `skin` op**: one bone per chain point. Named joints keep their names
    and other points are `c{chain}_{point}`. The radii come from the chain,
    and a skin with `mirror: "x"` gets `_l` / `_r` bones.
  - Extra `bones` may hang off the generated ones.
