# Seeing, reading and checking motion

- **Seeing motion**: `agent_views` with `clip` and `times: [0, 0.25, 0.5,
  0.75]` renders a **filmstrip**: each view at each time, tiled with labels.
  This is how to judge a walk. `time` gives a single moment.
  - `style: "bones"` overlays the skeleton.
  - `style: "weights"` with `bone` shows a heat map of that bone's weights.
  - `style: "trails"` (with `clip`; optional `bones`: names / `*` globs,
    default the moving end bones (hands, feet, head, tail tips) plus the
    root, at most 8; optional `time`, where the body is drawn, default 0)
    draws the model see-through and, per bone, the 3D path its tip (tail;
    a root's head) travels over the WHOLE clip: one colour per bone (legend
    in the image), dots at 24 even time steps that grow with time (so
    spacing shows speed and the growth shows direction), a big dot at
    t = 0, and the stretches where the tip is within 3 cm of y = 0 thick
    and white (a planted foot reads at a glance; a sliding one is a long
    white stretch). The framing includes the trails. The main use is
    `views: ["right"]` of a walk. `inspect curves` is the numeric twin.
- **Reading curves**: `inspect(curves: {clip?, bones?: [globs], channels?:
  ["height", "speed", "angle", "x", "z"], samples?: 120})` (headless, no
  renderer): per bone and channel the min / max with their times and the
  mean, the contact spans `[t0, t1]` where the tip is within 3 cm of the
  ground, and (with `image: true`) one image of small-multiple line plots (time on x, one row
  per channel, one coloured line per bone, the clip's key times as faint
  vertical ticks, planted bars under the plot). `height` = tip world y (m),
  `speed` = tip speed (m/s), `angle` = the bone's rotation from rest (deg),
  `x` / `z` = tip position (m). Default channels height, speed, angle;
  default bones as for trails. The JSON keeps at most 60 points per line.
  The maths is `src/inspect/motion.rs` (also used by the trails renderer).
- **Checking motion**: `verify`.
  - `weights: true`: every vertex of a smooth part is weighted, the weights
    sum to 1, and there are at most 4 bones per vertex.
  - `animation: {clip, samples: 16, collisions, floor, loop, stretch,
    foot_slide, balance, smooth, gait, volume}` runs at each sample time and fails
    naming the time and bones:
    - `loop`: the pose at 0 matches the pose at the end, and every bone
      tip's velocity just inside the seam (start vs end) agrees within 50%
      of the faster side or 2 longest-bone-lengths/s (a reversed or stalled
      motion at the seam fails);
    - `floor`: nothing sinks below y = 0;
    - `collisions`: a limb through the body;
    - `stretch`: skinning tears or collapses a triangle;
    - `volume: true | tolerance` (0.08): each smooth-bound closed part's
      posed volume over its rest volume stays within the tolerance at every
      sample (a joint that collapses or balloons); a failure names the part,
      the ratio, the time and the bone weighted at the most moved vertex,
      the PASS line gives the worst ratio;
    - `foot_slide: true | {bones?, contact_height 0.03, tol 0.01, speed?}`:
      while a foot bone's tip (tail) is within `contact_height` of y = 0
      its horizontal drift from where the run of planted samples began must
      stay under `tol`. `speed: [vx, vz]` is the character's ground velocity
      (m/s) for a walk in place: planted feet must move at `-speed`, and
      the slide is measured after adding `speed * dt`. With no `speed`, a
      `mocap` clip's removed travel (`Clip.speed`) is used when the clip has
      no root motion of its own. `bones` takes names
      or globs; `true` picks every `toe` bone, else every foot / paw /
      hoof / ball bone, else the lowest leaf bones. Reports the worst slide
      (bone, time span) and each foot's planted share of the cycle;
    - `balance: true | {margin 0, min_share 1, contact_height 0.02}`: at
      each sample the posed centre of mass (the `inspect mass` report)
      projected to the ground must lie inside the convex hull of the
      vertices within `contact_height` of y = 0, grown by `margin`. A
      sample with nothing on the ground is airborne (reported, not judged).
      Reports the worst overhang and time and the balanced share; a walk is
      not statically balanced mid-stride, so give `min_share` below 1;
    - `smooth: true | {max_accel, max_jerk}`: bone tips are differenced at
      60+ samples per second of clip (whatever `samples` is). By default a
      sample whose acceleration is over 8x the bone's median non-zero
      acceleration and over 5 m/s^2 is a pop ("pop at t=0.42 on `wrist_l`:
      38.0 m/s^2 against a median 3.1"); `max_accel` (m/s^2) and
      `max_jerk` (m/s^3) are absolute limits instead. Linear keys kink at
      every key, so pass `max_accel` for them.
    - `gait: true | {side?: both / left / right, tolerance?: degrees,
      speed?: [vx, vz], bones?: {thigh, knee, ankle, toe, wrist, hip,
      chest}}` (bone names take `{side}` = `l` / `r`; defaults
      `thigh_{side}` .. and `hip`, `chest`, as the `body` biped and
      `human` rigs name them): a biped walk measured against normative
      human gait (Perry & Burnfield, *Gait Analysis*, 2010; knot tables
      `NORM_THIGH` / `NORM_KNEE` / `NORM_ANKLE` in `src/anim/check.rs`).
      Each leg's cycle starts at heel strike: the start of a planted span
      (heel point within 5 mm of the ground, or toe tip within 4 mm of its
      rest height) that includes the heel. Angles are geometric, from
      posed joint positions in the plane of travel (so FK, IK and any bone
      frames measure alike): thigh = hip -> knee from vertical (+
      forward), knee = flexion between thigh and shank, ankle = shank vs
      ankle -> ball relative to the rest pose (+ dorsiflexion). Curves
      pass within a band (thigh 7, knee 10, ankle 7 degrees, about 1 SD;
      `tolerance` replaces all three) with 3% of the cycle timing slack;
      a FAIL names the value, percent of the cycle, phase, normal value
      and what is wrong, plus the worst in stance and in swing ("knee:
      38 deg at 30% GC (mid stance, left), normal 6 +- 10: the knee is
      too bent"). Also: `knee_extension` (never below -5), `timing`
      (cycle 0.9..1.3 s x sqrt(stature / 1.75) at a comfortable speed
      within +-15% of 1.35 x sqrt(stature / 1.75) m/s; beyond it the band
      scales by sqrt(edge / speed), cadence growing about with the square
      root of speed (Grieve & Gear 1966), stance 55..65% per leg,
      double support 8..32%), `stride` (speed x cycle over stature
      0.6..1.0; speed stated, else `foot_slide`'s, else the root motion,
      else a `mocap` clip's own speed, else measured from the planted feet; forward = the rest feet's
      facing), `step_width` (5..15 cm scaled), `pelvis` (hip head
      excursion 2..7 cm scaled, two peaks a cycle), `trunk` (chest lean
      under 10), `arms` (wrist fore-aft against the same-side ankle:
      correlation below -0.5, swing 10..50 cm, centred -6..12 cm from the
      hip joints) and `toe_clearance` (least toe lift over rest in mid
      swing 0.5..6 cm). Lengths scale with stature (hip joint height /
      0.53). A rig without the leg bones gets a `skipped` note, not a
      failure.
    All of them pose through the clip evaluator, so IK and constraints
    count. Example: `{"animation": {"clip": "walk", "foot_slide": {"bones":
    ["toe_*"], "speed": [0, 1.4]}, "balance": {"min_share": 0.5},
    "smooth": true, "gait": true}}`.
- **Inspecting a pose**: `inspect(pose: {clip, time, bones?})` gives bone
  world positions, and `measure` / `section` / `mass` / `thickness` then run
  on the posed model.
- **The window**: `set_camera(play: {clip, time?, speed?, paused?})` plays a
  clip. The human uses P (play/pause), `[` `]` (switch clip) and `,` `.`
  (step).
- **Export**:
  - Each skeleton becomes a joint node tree under a root node named after
    the skeleton, plus a glTF skin with inverse bind matrices.
  - Smooth parts get `JOINTS_0` / `WEIGHTS_0`; rigid parts become children of
    their joint.
  - Clips become `animations`, sampled at `fps` plus the key times. Bevy
    plays them with `AnimationPlayer`.
  - A `sway` also writes its settings to `extras.sway` on the skeleton root,
    for the runtime plugin, and every skeleton root carries `extras.clips`
    (`{clip: {loop, duration, root_motion?, events?}}`, the same as each
    animation's own extras): Bevy's loader does not expose animation extras,
    so the plugin reads this table. A clip's morph weights go on the body's
    own node (a second animation root), so a rigged character has one
    `AnimationPlayer` per root; `modeler_bevy::ModelerClips` plays them
    together.
  - `instance`, `merge`, `cull_contacts` and `lods` leave rigged parts
    alone.
