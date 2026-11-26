# `bind`

- **`bind`** attaches parts to a skeleton:
  - `bone: "forearm_l"` binds rigidly: a door on its hinge, a robot arm.
  - `mode: "smooth"` gives per-vertex weights from the bone envelopes,
    diffused over the mesh: two legs side by side don't bleed into each
    other. `blend` is the meters weights blend across a joint, and `paint:
    [{bone, region: {min, max}}]` forces a region onto one bone.
  - `mode: "auto"` on an lsystem rig gives exact weights along the branches,
    and every leaf card rides its twig's bone rigidly. Bind both:
    `"parts": ["oak", "oak_leaves"]`. On any other rig, `auto` is `smooth`.
  - `weights_from: "eve"` (smooth): clothing, armour, hair caps modelled
    over a bound body take each vertex's weights from the closest point of
    that part's surface (barycentric over the closest triangle), and its
    correctives too (the body's offsets at that point, same drivers), so
    the cloth follows the corrected skin; `blend` then smooths the weights
    over the part's own edges. Such a bind doesn't replace a body's
    implicit bind on the same rig. A sleeve over the human's arm keeps its
    offset from the skin within 0.5 mm through the walk and 2.9 mm at the
    elbow's limit (~107 degrees: LBS shortens an offset across the bend).
  - `correctives: true | false | {joints: ["elbow_*"], poses: [{bone,
    axis: "x", degrees: 120}], max: 128}` (smooth; on by default for a
    mesh with the human template's weights, off elsewhere): pose-driven
    corrective shapes, so plain linear blend skinning (what glTF and
    Bevy do) looks like a better deformer. For each key pose (human:
    elbow / knee 120, shoulder up 90 / forward 90, hip forward -100 /
    out 45, clavicle up 30, wrist +-60, thumb joints 45 / 40 / 60,
    finger joints 90, spine / neck twist, spine 30, jaw 20; other rigs:
    each limited axis at its ends, else x +-90) the reference is the LBS
    pose with the joint's blend zone relaxed toward the rest edge
    lengths (vertices split between the bone's subtree and the rest,
    owned by that subtree and its ancestors, two rings wider, and for
    limb bones over 8 cm also into their own side: the armpit and groin
    take skin from the arm and thigh), held to LBS elsewhere. The
    difference, carried back by the inverse blended matrix, is a sparse
    rest-space morph target. Deep bends get in-between keys (a third /
    half of the way, warm-started from the previous key; not the
    shoulder, whose single key measured better) whose weights are tents
    on the bone's twist angle about the key axis in its rest frame
    (`from` -> `degrees` -> `to`), so the correction follows the sweep
    with no pops (elbow: at most 1.3 mm change per 4 degrees). A guard
    checks each tent at a quarter, half and three quarters of the way
    and pulls back offsets where the blend pinches or stretches an edge
    worse than LBS (a relaxed knuckle key slides skin across the fold;
    half of it at half the angle collapsed edges: fingers curled 18
    degrees were 1.91x with correctives, 1.24x now). Every posing path
    (views, verify, inspect, the window) applies them before skinning.
    Exported after the `shapes` as ordinary morph targets with the
    driver table in the node's and mesh's `extras.correctives`
    (`[{target, name, bone, axis, degrees, from, to, rest_local}]`);
    clip `morphs` samplers write 0 in their slots.
    Bevy allows 256 morph targets per mesh (shapes + correctives), and its
    morph-texture path (WebGL2) caps vertices x targets x 12 floats at
    2048^2; the human (14.5k vertices, 130 targets) fits the first, not
    the second, so on WebGL2 use `correctives: {"max": 24}` or fewer.
