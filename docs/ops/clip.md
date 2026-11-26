# `clip` and `pose`

- **`clip`**: keyframed motion.
  ```json
  {"type": "clip", "name": "walk", "skeleton": "rig", "duration": 1.0,
   "tracks": {"thigh_l": {"rotate": [[0, {"x": 25}], [0.5, {"x": -25}], [1, {"x": 25}]]},
              "hip": {"translate": {"wave": {"axis": "y", "amount": 0.03, "period": 0.5}}}},
   "mirror": {"thigh_r": {"from": "thigh_l", "phase": 0.5}}}
  ```
  - Keys are `[t, value]`: rotations in any rotation form, translate in
    meters, scale as a factor.
  - Interpolation is `smooth` (Catmull-Rom; the default), `linear` or `step`.
  - `wave` generates a periodic channel.
  - `mirror` + `phase` copies a track to the other side, mirrored and half a
    cycle later, so a walk is half-authored.
  - `loop` defaults to true.
  - `morphs` keys morph target weights (a `human` body's `shapes`; the
    part is the body's name): `{"eve": {"smile": [[0, 0], [0.5, 1]],
    "jaw_open": 0.3, "breath": {"wave": {"amount": 0.2, "period": 4}}}}`
    (keys with the clip's `interp`, a constant or a wave; weights are not
    clamped). Unknown parts and shapes error with the closest name. Deltas
    apply before skinning, normals turn with the faces, and a clip whose
    weights are all 0 poses bit-identically to one without. A layer
    (`over`) replaces its base's weights (by `weight`) or, `additive`,
    adds; `pose` items take `morphs` too (`{"eve": {"smile": 1}}`) so
    `poses` keys carry expressions (0 where a pose leaves a shape alone).
    Talking and blinking over a walk:
    ```json
    {"type": "clip", "name": "talk_walk", "skeleton": "eve_rig", "over": "walk", "additive": true,
     "morphs": {"eve": {"jaw_open": [[0, 0], [0.15, 0.6], [0.3, 0.1], [0.45, 0.7], [0.6, 0]],
                        "smile": [[0, 0.2], [0.5, 0.6], [1, 0.2]],
                        "blink": [[0, 0], [0.45, 0], [0.5, 1], [0.55, 0], [1, 0]]}}}
    ```
    (with `shapes` `jaw_open`, `smile` and `blink` on the body, made
    from its expression units). Exports write a glTF `weights` channel
    per morphed node (every target's weight per keyframe, in
    `extras.targetNames` order), which Bevy's loader plays; a clip's
    morphs export only when its skeleton has bound parts. The window
    plays them and shows a lane per `part:shape`.
  - **`ik`** plants feet and reaches hands by naming targets instead of
    keying angles. Each entry is keyed by the END bone whose **head** (its
    joint) is driven to `target`, solved after the tracks by turning the
    `chain` bones above it (default 2; the end bone itself only rides,
    unless `rotate`). On a `body` biped rig give `ankle_l` / `ankle_r` for
    feet (chain 2 bends `thigh` and `knee`) and `wrist_l` / `wrist_r` for
    hands (bends `shoulder` and `elbow`).
    - `target`: `[x, y, z]` world meters, a part anchor (`"crate:top"`),
      or keys `[[t, [x, y, z]], ...]` interpolated like translate keys.
    - `pole`: a point or anchor the middle joint bends toward (two-bone
      chains). Without one the chain keeps the bend the FK pose has, else
      the rest pose's, so a knee never flips. Longer chains use FABRIK
      from the FK pose (no pole).
    - An unreachable target straightens the chain toward it; bones never
      stretch.
    - `weight`: 0..1 or keys; 0 is the FK pose exactly, 1 full IK, so a
      foot can be planted for part of the cycle.
    - `rotate`: keys of the end bone's world rotation as an offset from its
      rest frame (`[[0, {"x": 0}]]` keeps a foot level).
    - `mirror: {"ankle_r": {"from": "ankle_l", "phase": 0.5}}` copies an
      ik entry (and the track, if any) with target and pole x negated.
    - Exports sample every IK-moved bone at `fps`; the Animate dock shows
      them as a line.
  - **Spins** (`{"euler": [[0, {"y": 0}], [1, {"y": 360}]], "interp"?}`
    on `rotate`): ordinary rotation keys take the short way, so a full
    turn can't be keyed; Euler keys interpolate each axis's degrees as
    numbers (composed x, then y, then z, like `{x, y, z}`), across a
    loop's seam too, so this one spins at constant speed forever. Mirror,
    limits and export sampling work as for any rotation.
  - **Poses**: a `pose` item is a static offset-from-rest per bone,
    `{"type": "pose", "name": "crouch", "skeleton": "guy_rig", "mirror":
    true, "bones": {"hip": {"translate": [0, -0.3, 0]}, "thigh_l":
    {"rotate": {"x": -70}}, "knee_l": {"rotate": {"x": 110}}}, "ik"?:
    {"ankle_l": {"target": [0.1, 0.085, 0.1]}}}`. `mirror` fills each
    `_r` bone (and IK entry) from its `_l` twin and back where it isn't
    given; `ik` is solved once when the pose builds (targets: points or
    anchors). A clip keys whole poses with `"poses": [[0, "stand"], [0.4,
    "crouch"], [1, "stand"]]` (`"rest"` is built in): every bone either
    pose touches gets a key at each pose key, interpolated with the clip's
    `interp`; an explicit track channel on a bone (rotate, translate or
    scale) replaces what the poses key on that channel, and a `mirror`
    onto a bone replaces its pose keys. `agent_views` / `inspect pose` /
    `verify animation` take a pose's name as a `clip` (a one-second static
    clip); poses are never listed or exported as clips.
  - **Layers**: `"over": "walk"` makes this clip a layer on a keyed base
    clip of the same skeleton (duration defaults to the base's and must be
    a whole number of its cycles when both loop). Its tracks replace the
    base's on their bones, blended by `weight` (0..1, a number or keys
    `[[t, w], ...]`), or with `"additive": true` compose on top: rotation
    after the base's (about the bone's posed axes), translation added,
    scale multiplied, scaled by `weight` (0 = the base exactly). `bones`
    (`*` globs) limits which bones the layer touches. Order at every
    sample: the base's tracks, the layer's on top, then once on the
    result: limits, every level's IK (base entries first), look_at; a
    `sway` over the layered clip comes last. A layer inherits the base's
    `root_motion` unless it sets its own.
  - **Root motion**: `"root_motion": true` (bone `hip`, else the first
    root bone; axes `xz`) or `{"bone": "hip", "axes": "xz", "yaw"?:
    true}` names a ROOT bone whose translate on those world axes (and turn
    about y, with `yaw`) is the character's travel. In the modeler (views,
    verify, the window) the clip plays travelling, as keyed. Exports remove
    the travel from that bone and write the animation's
    `extras.root_motion = {bone, axes, yaw, delta: [x, y, z], yaw_degrees,
    duration, samples: [[t, x, y, z, yaw], ...]}` (from t = 0) for the
    game to move the entity. `verify animation` judges `loop` in place
    and `foot_slide` in the world with the travel carried across the seam,
    so planted feet pass without a `speed`. The root bone's translate keys
    interpolate through the seam with the loop's travel added to the
    wrapped neighbours (no bend at the seam).
  - **Root motion with IK**: in a looping root-motion clip, keyed `ik`
    targets are authored in WORLD space for one loop, travel included: a
    planted foot keyed at a fixed point, the next stance one stride ahead
    (the key at t = duration = the key at 0 plus the loop's root delta).
    The clip stores them with that trend removed and adds the loop's
    progress back when sampling, so playback carries targets (and poles,
    which travel with the body) across the seam exactly as it carries the
    root, a mirror entry stays relative to the body, and in-place sampling
    (verify, export) moves targets back with the root. A constant target
    point stays fixed in the world. `yaw` root motion does not turn the
    targets.
  - **Events**: `"events": [[0, "footstep_l"], [0.5, "footstep_r",
    {"volume": 0.8}]]` (times within the duration, names non-empty) go to
    the animation's `extras.events` as `{time, name, data?}`;
    `scene_summary`'s rig line counts them.

  Stand, crouch, jump with poses (`guy` a `body` biped):
  ```json
  {"type": "pose", "name": "crouch", "skeleton": "guy_rig", "mirror": true,
   "bones": {"hip": {"translate": [0, -0.25, 0]}, "spine": {"rotate": {"x": 15}}},
   "ik": {"ankle_l": {"target": [0.1, 0.085, 0.05], "pole": [0.1, 0.5, 1.0]}}}
  {"type": "pose", "name": "air", "skeleton": "guy_rig", "mirror": true,
   "bones": {"hip": {"translate": [0, 0.5, 0]}, "thigh_l": {"rotate": {"x": -30}},
             "knee_l": {"rotate": {"x": 50}}, "shoulder_l": {"rotate": {"z": 60}}}}
  {"type": "clip", "name": "jump", "skeleton": "guy_rig", "duration": 1.2, "loop": false,
   "poses": [[0, "rest"], [0.3, "crouch"], [0.6, "air"], [0.95, "crouch"], [1.2, "rest"]],
   "events": [[0.35, "takeoff"], [0.95, "land"]]}
  ```
  A wave layered over a walk (only the right arm; the legs keep walking):
  ```json
  {"type": "clip", "name": "walk_wave", "skeleton": "guy_rig", "over": "walk",
   "additive": true, "bones": ["shoulder_r", "elbow_r"], "weight": 1,
   "tracks": {"shoulder_r": {"rotate": [[0, {"z": 0}], [0.25, {"z": -150}], [1, {"z": -150}]]},
              "elbow_r": {"rotate": {"wave": {"axis": "z", "amount": 25, "period": 0.5}}}}}
  ```

    A biped walking in place with planted feet:
    ```json
    {"type": "clip", "name": "walk", "skeleton": "guy_rig", "duration": 1.0,
     "tracks": {"hip": {"translate": {"wave": {"axis": "y", "amount": 0.02, "period": 0.5}}}},
     "ik": {"ankle_l": {"target": [[0, [0.1, 0.085, 0.2]], [0.5, [0.1, 0.085, -0.2]],
                                   [0.75, [0.1, 0.2, 0]], [1, [0.1, 0.085, 0.2]]],
                        "pole": [0.1, 0.5, 1.0], "rotate": [[0, {"x": 0}]]}},
     "mirror": {"ankle_r": {"from": "ankle_l", "phase": 0.5}}}
    ```
