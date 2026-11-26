# The `human` body preset

  - **`preset: "human"`** is a parametric *template*, not a field: an
    authored quad base mesh (MakeHuman's CC0 core assets) plus a basis
    of morph targets, re-derived like any op. `{"type": "body", "name":
    "eve", "preset": "human", "gender": 0, "age": 30, "muscle": 0.5,
    "weight": 0.5, "height": 1.63, "proportions": 0.5, "ethnicity":
    {"african": 0.2, "asian": 0.3, "caucasian": 0.5}, "shape":
    {"nose-width1": 0.3, "head-fat": -0.4}, "expression":
    {"mouth-corner-puller": 0.6}, "detail": 1, "parts": ["eyes",
    "teeth", "tongue"]}`.
    - `gender` 0 female .. 1 male (default 0.5); `age` in years (> 1;
      default 25) or MakeHuman's 0..1 (baby 1 y = 0, child 11 y =
      0.1875, young 25 y = 0.5, old 90 y = 1); `muscle`, `weight`
      (body fat), `breast_size`, `breast_firmness` 0..1 (default 0.5);
      `proportions` 0..1 (0.5 neutral, above idealized, below uncommon);
      `ethnicity` weights (normalized, default equal). These blend the
      macro targets multilinearly between the nodes their files are
      named after. `height` is the final stature in meters: the template's
      own height targets are searched for it (they keep its proportions,
      a tall adult is not a scaled child), then it is scaled exactly;
      without `height` the figure keeps the template's natural size
      (about 1.59 m female, 1.73 m male at 25 y).
    - `shape`: any detail target by its MakeHuman modifier name
      (`nose-width1`, `head-fat`, `l-foot-scale`, `measure-waist-circ`,
      `r-eye-bag-decr-incr` where a target has several pairs: `{target}-
      {min}-{max}`), -1..1 between its decrease and increase files
      (one-sided keys 0..1); `expression`: face units 0..1
      (`mouth-open`, `mouth-corner-puller` = smile, `mouth-elevation`,
      `eye-left-closure`, `eyebrows-left-inner-up`, ... 34 units).
      Unknown names error with close matches.
    - `detail` 0..2 is Catmull-Clark over the quad cage (0: 26,756
      triangles; 1: 107,024; 2: 428,096), skin weights, morph deltas and
      uvs carried through the same stencils; the subdivided body is
      scaled back to the cage's stature (exactly `height`) on y = 0, and
      the rig's joints are read from that subdivided surface. `tris`
      decimates the body last (without `bake` it starts from the
      coarsest level with at least `tris` triangles, and a budget at or
      above that count is not decimated): weights, morphs and joints come
      from the nearest dense vertex, and the **template uvs are kept**
      (each light triangle takes its uvs from the nearest dense
      triangle's uv island, seams split vertices; a few triangles
      straddling a seam stretch there). Local `warps` (inflate / push /
      pinch / flatten / smooth) are vertex displacements on the cage,
      symmetric about x = 0 like the other presets, centred on a point, a
      joint name (`"chest"`, `"eve:chest"`), another part's anchor
      (`"desk:top"`, a joint of another body) or anchor words on the
      body's own box (`"top-front"`); field warps (twist, bend, taper,
      displace) are refused. Warps don't move the rig.
    - `measure: {"chest": 1.0, "waist": 0.85, "hips": 1.0, "neck": 0.38,
      "upper_arm": 0.31, "thigh": 0.58, "calf": 0.37}` (meters; also
      `underbust`, `wrist`, `knee`, `ankle`) solves MakeHuman's
      `measure-*-circ` targets one by one (a bisection on each slider,
      two rounds) so a tape round the body reads that girth: the convex
      perimeter where a plane through the target's most-moved vertices
      (level for the torso, square to the limb for arms and legs, left
      side) cuts the skin that target shapes. It lands within 0.01% when
      the slider can reach the girth; each slider spans what MakeHuman's
      does (the waist only about +14% over the default), and a girth
      beyond is clamped to the slider's end: raise `weight` / `muscle`
      first. The tape is this preset's own, not ANSUR's landmarks.
      Defaults run ~12-18% under ANSUR II torso medians
      (`default_adults_against_ansur_medians`): use `measure` or
      `weight` for a target build.
    - The body is closed: the lips meet, and behind them is a closed
      inner mouth cavity, so an open mouth (the `mouth-open` unit or the
      `jaw` bone) shows the teeth and tongue inside it. Extras are
      separate parts: `<name>_eyes` (spheres fitted to the template's eye
      helpers, looking along +z, each on its `eye_l` / `eye_r` bone),
      `<name>_teeth` (upper on `head`, lower on `jaw`, holes capped),
      `<name>_tongue` (on `jaw`; expression units carry it with the lower
      teeth); `parts: []` for none. Teeth (enamel, light clearcoat) and
      tongue (wet pink) wear built-in card-domain looks over their own
      uvs, like the eyes; a `material` item replaces them. No eyelashes or eyebrows (the
      template's are hair cards, not kept).
    - **Eyes look right without a material**: `<name>_eyes` wears a
      built-in card-domain material (`human_eye`, embedded in the part):
      the eye's uvs are azimuthal about the pupil (uv [0.5, 0.5] is the
      front pole; iris radius 0.08 in uv), drawn as sclera with faint
      veins, an `eye_color` iris (`"#3a6ea5"` or [r, g, b]; default a mid
      brown) with noise fibres, a dark limbus ring, a `pupil`-scaled pupil
      (1 = normal, 0.4..2.2) and a clearcoat cornea. A `material` item on
      the eyes replaces it (`human::eye_graph` is the node recipe, for
      `material_ops` with domain `card`, tile 1).
    - The rig is a game rig of 58 bones named like the biped's: hip,
      spine, chest, neck, head, jaw, eye_l/r, clavicle, shoulder, elbow,
      wrist, hand (a knuckle joint the fingers hang from), thumb_1..3,
      index / middle / ring / pinky_1..3, thigh, knee, ankle, ball, toe
      `_l`/`_r` (bone named by the joint at its head, real tails from the
      template). Its weights start from the template's own (not
      envelopes), merged from MakeHuman's 163-bone skeleton into the kept
      ancestors (twist bones into their limb bone, face bones, extra
      spine, toes onto `ball`), then reworked for deformation: between
      the legs each thigh's weight fades to zero toward the midline over
      8 cm below the hip joints (the rest goes to `hip`; not at the front
      groin crease, which a wide ramp tears), the body's
      weights diffuse 8 half-steps over the skin (digits excluded, so
      fingers keep their own) and more round the armpits, elbows, wrists
      and knees; the mouth helpers ride one bone each. `verify animation`
      stretch, game rig vs `rig: "full"` (the source weights): walk
      (thigh +-30, knee 50, shoulder 25) 1.58 vs 2.90, arms out to
      horizontal (shoulder z 45) 1.98 vs 2.74, knee 120 1.48 vs 2.22,
      wrist 60 1.27 vs 1.63, neck turn 60 1.46 vs 2.79, spine 30 1.14 vs
      1.29; still over 2: arms raised 90 over the A-pose 2.71 (4.20),
      arms forward 90 2.64 (3.93), elbow flexed 120 3.72 (5.12), crouch
      (thigh -100, knee 120) 2.70 (6.18), fist (fingers 80/90/90) 4.72
      (4.72), jaw open 20 10.49 (10.66: the lip corners, where the closed
      mouth slit tears; the `mouth-open` expression opens it cleanly).
      **Correctives** (on by default for the human, see `bind`) bring
      those to (stretch, off -> on): elbow 120 4.37 -> 1.26 (16-sample
      sweep 1.26), knee 120 1.48 -> 1.23, crouch 2.82 -> 1.87, sit (thigh
      -90, knee 90) 2.68 -> 2.14, arms up 2.70 -> 2.11, arms forward 2.14
      -> 1.74, arms out 1.98 -> 1.68, fist 5.80 -> 1.96 (2.00 right),
      fingers curled 18 1.17 -> 1.24, thumb opposition 2.61 -> 1.80,
      shrug (clavicle 30) 2.66 -> 1.73, jaw 20 10.49 -> 4.56, walk 1.57,
      wrist 1.15, neck turn 1.45, nod 1.22, spine 1.17, spine twist 45
      (two bones) 1.29; twists without twist bones stay under 2 with the
      volume kept (forearm pronation / supination 80 1.59 / 1.52, upper
      arm 70 1.66, thigh 40 1.63; body volume >= 0.993), so the merged
      game rig has no twist bones. Still over 2: the raised arms' armpit
      (the skin there must stretch with no clavicle rhythm), the sitting
      groin (between two thigh keys) and the jaw: the template's mouth
      slit is closed, so the corner edges are tiny and even its own
      `mouth-open` unit stretches them 12x (6.5x at 0.5); open a mouth
      with that expression rather than the jaw bone when it matters.
      (`human::tests::corrective_stretch_report` prints the table.) 130
      targets, ~24k sparse offsets, 50-100 ms of build time, recomputed
      (and cached) per mesh, weights and rig, since they depend on the
      shape. They come most valuable first, so a small `max` keeps the
      best: elbows and knees (with their in-betweens), hips, shoulders,
      spine, clavicles, neck, wrists, jaw, thumbs, fingers last; at `max:
      24` (WebGL2) walk, elbow, knee, arms stay as with all of them,
      crouch is 2.25 and the hands fall back to LBS.
      `rig: "full"` keeps the whole skeleton (face bones too, names
      `upperarm01.L` -> `upperarm01_l`) with the source weights. Joint
      names are anchors (`"eve:wrist_l"`). The bind covers the body and
      its extras.
    - **What a rotation means on each bone** (game rig). Legs, spine,
      neck, head and jaw are world-aligned (identity rest), so biped clips
      keep their meaning: `thigh_l {"x": 30}` swings the leg back (-x
      forward), `knee_l {"x": 50}` bends the shin back, `ankle_l {"x":
      15}` points the toes down, `ball_l {"x": -20}` bends the toes up,
      `spine {"x": 30}` bends forward, `neck {"y": 60}` turns the head,
      `jaw {"x": 20}` opens the mouth. Arms and digits have anatomical
      rest frames (`human::bone_frames`, applied by the rig): local +y
      runs along the bone and **+x flexes it on both sides** (shoulder:
      raises the arm forward; elbow: closes; wrist, fingers, thumb: curl
      toward the palm); y twists about the bone and z abducts, both
      mirrored left / right like every mirrored track (`shoulder_l {"z":
      45}` raises the left arm out to the side, `shoulder_r {"z": -45}`
      the right; `{"z": -36}` on the left brings an A-pose arm down to the
      side). x 0 is the template's A-pose, whose elbow is already bent
      ~43 degrees and knee ~7. **Limits** on every bone come from the
      normal adult ranges of motion (AAOS / Norkin & White goniometry;
      the table is `human::ROM`), measured from anatomical zero (straight
      leg, straight arm, arm at the side, straight finger) and shifted by
      the template's rest bend, so a knee straightens fully: hip x
      -120..45 (flexion is -x; extension 30 plus 15 for the pelvic tilt
      the merged hip bone can't show), abduction 45 / adduction 30, rotation
      +-45; knee -5..150 from straight (x -12..143 from rest); ankle
      dorsiflexion 35 (weight-bearing) / plantarflexion 50; toes -70..40;
      spine + chest share thoracolumbar flexion 80 / extension 25 /
      lateral 35 / rotation 50; neck + head cervical flexion 50 /
      extension 70 / rotation 80 / lateral 45; jaw 0..30; clavicle
      elevation 40; shoulder flexion 180 / extension 60 / abduction 180
      (from the side) / rotation +-90; elbow -3..150 from straight,
      pronation / supination 85 (elbow y); wrist flexion 80 / extension 70
      / deviation 30; finger MCP -20..95, PIP -5..115, DIP -10..90 from
      straight; thumb CMC, MCP, IP. No example pose or walk key is
      clamped (`limits_follow_the_rom_table_and_keep_the_examples`). IK on
      `wrist_l` (chain 2) reaches with the elbow keeping its rest bend
      plane; for a reach in front of the chest give a `pole` out and back
      (`[0.5, 0.9, -0.4]` on the left), or the elbow tucks in front of the
      body; an overhead target straightens the arm toward it.
    - **Poses** (`examples/human_poses.json`, `pose` items on `eve_rig`,
      checked in renders, mirror on): `stand` (arms down at the sides,
      elbows and fingers relaxed: the natural standing pose over the
      A-pose rest), `t`, `crouch`, `sit` (hip lowered, thighs level: put
      a seat under it), `wave` (right hand up), `point` (right index
      forward), `fist`. Key them in clips with `"poses": [[0, "stand"],
      ...]` or layer a clip over them.
    - A walk to start from: `examples/human_walk.json` (a plain
      `apply_ops` batch), generated from published normative gait
      kinematics by `python3 -I examples/gait_gen.py --out
      examples/human_walk.json`: 112 steps/min (1.07 s cycle) in place at
      1.19 m/s, stride 1.27 m (0.78 x stature). The table's thigh / knee
      angles on the rig's own leg lengths give the ankle path; stance rolls
      heel -> flat foot -> heel rise at 31% about the ball (toes flat up to
      40 degrees, then on the tip) -> toe-off at 62%, planted at exactly
      the walking speed; the pelvis sits where the leading leg has the
      table's knee (2.7 cm bob, lowest in double support, highest in mid
      stance), with +-4 degrees rotation and obliquity, +-2 cm sway,
      thorax and head counter-rotating, 10 cm step width, and arms swinging
      opposite the legs from the `stand` pose. `ankle_l` IK (mirrored to
      `ankle_r` at phase 0.5) with `rotate` keys for the foot pitch,
      `ball_l` for the toes; the clip starts at mid stance of the left
      leg. Stance knee straightens to ~7.5 degrees, toe clearance 1.2 cm.
      It passes `verify animation` loop, stretch 2 (1.91x), floor,
      volume, `foot_slide` `{"speed": [0, 1.1853], "contact_height":
      0.012}` (the toe tip sits 8 mm up at rest and clears the floor by
      ~1.2 cm in mid swing, so the default 3 cm reads swing as planted),
      `smooth` and every `gait` measure. Other files, same generator:
      `human_walk_slow.json` (`--cadence 90`, 0.95 m/s),
      `human_walk_brisk.json` (`--cadence 125`, 1.32 m/s),
      `human_walk_travel.json` (`--travel`: `root_motion` {bone hip, axes
      z}, feet planted at fixed world points; checks need no `speed`).
      The walk is fitted to ONE body: the generator reads the rest joints
      of the default `human` preset, so a body with other `height` /
      `gender` / shape needs its own (`--rest REST.json --body '{"height":
      1.65, "gender": 0}'`, REST.json = the JSON of `inspect(pose: {clip,
      time: 0, bones: [hip, thigh_l, knee_l, ankle_l, ball_l, toe_l]})` on
      a clip that keys nothing; `human_walk_outfit.json` is that for the
      outfit example's body). Embedding a walk made for another body puts
      the IK targets out of reach: loop, foot_slide and smooth then fail.
      Limits: the stride is the table's (a stated `--speed` changes it but
      pushes thigh / ankle off the norms on this rig, so speed comes from
      cadence), and the pelvis bob stays under the normal 4-5 cm: at this
      stride the leg is at full reach in mid stance and both legs must
      reach the floor in double support.
    - `shapes: {"smile": {"expression": {"mouth-corner-puller": 1}},
      "wide": {"shape": {"nose-width1": 1}}}`: named morph targets, the
      item's own base with those values swapped in, as per-vertex deltas;
      exported as glTF morph targets (`primitives[].targets`, mesh
      `weights` all 0, `extras.targetNames`). Keying them from a clip is
      not implemented yet.
    - Without a `material` item the body wears a built-in skin look
      (`human_skin`, `human::look::skin_graph`: card domain over the
      template uvs, mottled tone, lips / nails / areolae tinted from
      their zones, a faint redness on cheeks, nose, ears, knuckles, knees
      and elbows, an oilier T-zone and rougher knees and elbows,
      transmission 0.3), so it passes `materials: "pbr"`. `skin_tone`: 0
      (very light) .. 1 (very dark) along a melanin ramp, or a colour
      (`"#8d5524"`); the tints follow it. A `material` item replaces the
      whole look.
    - **Landmarks** (`human::landmarks`): named template vertices found
      from the data (the eye-closure units give the lid margins, the
      mouth-corner puller the mouth corners, the ear / nipple / navel
      detail targets those, the midline profile nose, lips and chin), so
      they follow every shape, expression and pose. They are anchors on
      the body like its joints (`"eve:brow_l"`, `frame`, move pivots,
      `place at`, warp `at`): `brow_inner_l/r`, `brow_peak_l/r`,
      `brow_outer_l/r`, `brow_l/r`, `eye_corner_in_l/r`,
      `eye_corner_out_l/r`, `lid_upper_l/r`, `lid_lower_l/r`,
      `nose_tip`, `nose_bridge`, `mouth_corner_l/r`, `lip_top`,
      `lip_bottom`, `chin`, `ear_top_l/r`, `crown`, `nipple_l/r`,
      `navel`. Loops (internal, for cards): `brow_l/r`, `lid_upper_l/r`,
      `lid_lower_l/r`, `hairline`.
    - **Zones** (per-vertex weights): `lips`, `face`, `scalp`, `brows`,
      `nails`, `areola`, `hands`, `feet`, `ears`, `nose`, `cheeks`,
      `knees`, `elbows`, `knuckles`, `tzone`. The material node `zone`
      (`{"type": "zone", "name": "z", "zone": "lips", "soft"?: m}`) is 0..1
      over the template's uv layout: use it in a card-domain material
      (`tile` 1) on the body to paint by region, e.g. lipstick and nail
      colour: `{"type": "zone", "name": "lips", "zone": "lips"}`,
      `{"type": "blend", "name": "c", "a": "skin_col", "b": "#a3102a",
      "opacity": "lips"}`. In the object domain it reads 0; on parts that
      don't use the template uvs it means nothing.
    - **Brows and lashes** (on by default): `<name>_brows` (a tapered
      card strip on each brow curve, alpha-cutout hairs) and
      `<name>_lashes` (curled strips on the upper and lower lid margins).
      `brows: false | {thickness 0.008, arch -1..1, color}`, `lashes:
      false | {length 0.009, curl 0..1, color}`. Every card vertex is tied
      to a body vertex (its skin weights and `shapes` deltas), and the
      cards are rebuilt from the evaluated body, so they follow
      expressions (a blink closes the lashes with the lid), poses and
      morph targets. Default brow colour: the hair colour, darker.
    - **Hair** (`human::hair`): `hair: "bob"` or `{style, length?,
      color?, part?: left / right / middle / none, fringe?: 0..1,
      volume?: 0..1, cards?, tris?, seed?}`. Styles: `bald`, `buzz`,
      `short` (the default when `hair` is omitted), `crop`, `bob`,
      `long`, `ponytail`, `bun`, `afro`, `curly`. Parts: `<name>_hair`
      (strand cards: curved strips rooted on the scalp zone, groomed away
      from the part, then down by gravity, kept off the body and round
      the face, a fringe stopping above the brows; alpha-cutout strand
      texture darker at the root, anisotropic highlight) and
      `<name>_hair_cap` (the scalp zone pushed out ~1 mm, its hairline
      feathered by the cutout; for `afro` / `curly` it is a thick lumpy
      mass dressed with curly cards; `bun` adds a ball). `buzz` is the
      cap alone, as `<name>_hair`. About 3-12k triangles by style; `tris`
      caps the strand cards. Hair is skinned rigidly through its root
      body vertex (the head), so it follows a walk; there are no hair
      bones yet, so `sway kind: "spring"` cannot swing it. The garment
      `hair_*` presets still exist (solid caps) but `hair` supersedes them.
    - The template file is `data/human/human.bin` (14.5 MB; packed by
      `cargo run --example pack_human -- <makehuman>/makehuman/data`,
      see `data/human/README.md`). It is looked for at
      `$MODELER_HUMAN_ASSET`, then beside the running binary
      (`human.bin` or `data/human/human.bin`; the installed build in
      `bin/modeler/` needs `bin/modeler/human.bin`), then this
      repository's `data/human/`, then `data/human/` under the
      working directory; a missing file is an error naming every path
      tried. Tests that need it skip with a message.
