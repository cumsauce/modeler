# `mocap`

- **`mocap`**: motion capture from a BVH file, retargeted onto a skeleton
  and lowered to an ordinary clip named `name` (dense rotate keys per mapped
  bone at `fps`, the root's translation, foot-planting `ik` entries and
  `footstep_l` / `footstep_r` events), so everything that plays, layers
  (`over`), exports and verifies clips takes it unchanged. Hand-keyed
  human motion comes out stiff; use this for people.
  ```json
  {"type": "mocap", "name": "walk_mc", "skeleton": "eve_rig", "file": "mocap/walk.bvh",
   "range": [0.5, 2.5], "loop": "blend", "blend": 0.25, "facing": "auto"}
  ```
  - `file` is project-relative and read like `ops_file` (no `..` or
    symlink out of the project); the server parses it on apply (errors
    name the line) and stores its hash, and a changed file re-derives the
    clip. Where the file can't be read (another project), the clip is the
    rest pose with a note. `bvh` takes the text inline instead.
  - **BVH**: ROOT / JOINT / End Site, OFFSET, CHANNELS (3 or 6, any order),
    MOTION rows; rotations compose in the written order as intrinsic
    rotations; tabs, CRLF, `1e-3` and trailing spaces are fine. Units are
    inferred from the skeleton's height (m, cm, inches, mm) and only
    reported: the root's translation is scaled by target / source leg
    length (thigh joint to the ground; else hip height), so units cancel;
    `scale` overrides that factor.
  - **Mapping** (`map: "auto"`, the default) by name, ignoring case,
    namespaces (`mixamorig:`), `Bip01` prefixes and `_` / `.` / spaces:
    CMU / Motionbuilder and Mixamo (`Hips, LeftUpLeg, LeftLeg, LeftFoot,
    LeftToeBase, LeftToe_End, Spine, Spine1, Spine2, Neck, Head,
    LeftShoulder, LeftArm, LeftForeArm, LeftHand, LeftHandIndex1..3`),
    3ds Max biped (`Bip01 L Thigh / Calf / Foot / Toe0, Bip01 L Clavicle /
    UpperArm / Forearm / Hand / Finger0..42`, `Pelvis`), CMU ASF names
    (`lfemur, ltibia, lfoot, lhumerus, lradius, lwrist, lowerback`) and
    `thigh_l` / `upperarm_l` styles, onto `hip, spine, chest, neck, head,
    jaw, clavicle_*, shoulder_*` (upper arm), `elbow_*` (forearm),
    `wrist_*` (hand), `thigh_*, knee_*` (shin), `ankle_*` (foot), `ball_*,
    toe_*`, `thumb_1_*..pinky_3_*`. Spine segments between hips and neck go
    to `spine` (first) and `chest` (last); joints left unmapped (Spine1 of
    three, `LHipJoint`, ...) ride the mapped bone below them, since the
    retarget is in world space. `map: {"chest": "Spine2", "jaw": ""}`
    overrides entries ("" unmaps), `{"only": {...}}` maps just those. The
    apply result lists the mapping and the unmapped joints.
  - **Retarget** in world space: the source rest pose (BVH zero pose:
    T-pose, A-pose, arms down) is turned to face +z, and per mapped bone
    the rotation taking its rest direction (joint to the mapped child's
    joint) onto the target's (head to the mapped child's head, else to its
    tail) is composed with the source bone's world rotation from rest, so
    each target bone points where its source bone points (twist carried),
    then converted to the bone's local offset from its own rest frame (the
    human's arm and finger frames included). Joint `limits` clamp as
    usual (a limit the source exceeds bends the bone off the source's
    direction).
  - **Travel**: `root_motion` `"in_place"` (default) removes the average
    travel (keeping sway) and stores its speed on the clip
    (`Clip.speed`, reported); `true` / `{"axes": "xz"}` keeps it and sets
    the clip's `root_motion` on the root bone. `facing: "auto"` turns the
    motion so the travel goes to +z, a number turns it that many degrees.
  - **Feet** (`feet`, on by default): contact spans are where an ankle is
    within `contact_height` of its lowest and slower than `contact_speed`;
    each span holds the ankle at its mean position with a 2-bone IK entry
    (weight blended over 3 frames, foot rotation kept), and where the leg
    can't reach (longer source legs, a knee that can't straighten past its
    rest bend) the hips are lowered. `hands` does the same for wrists (off
    by default). `mirror: true` swaps left and right; `speed` plays it
    faster; `range` takes seconds of the file.
  - **Loop**: `loop: true` loops as is (the result reports the seam in
    degrees / mm); `"blend"` crossfades the last `blend` seconds into the
    start (the clip gets that much shorter) so the seam is exact.
  - The apply result reports duration, frames, units and scale, the
    mapping, the travel, contact spans per foot, the worst planted slide
    (measured on the solved clip) and the loop seam. `modeler::mocap::bvh::
    from_clip` writes any clip as BVH (round trips within 2 mm).
