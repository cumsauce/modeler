# inspect and probe_ray

- `probe_ray(origin, dir, all_hits?)`: the first part hit (or `all_hits`:
  every part, nearest first). Proves openings are open and fills are solid.
  Runs on the model itself, so it works headless too.

- `inspect(measure?, section?, mass?, thickness?, proportions?)`: numeric senses, any
  combination per call.
  - `proportions: {parts?}`: for parts with joints (`body`): every bone
    chain's joint-to-joint lengths (`hip > spine > chest: 152 mm, 183
    mm`) and named ratios: biped head / height (crown to chin; 1/7.5 =
    0.133 for an adult), leg / height (hip joint to the floor), arm span /
    height (fingertip to fingertip); quadruped foreleg, hip-chest body,
    tail and head-snout against the shoulder height (the back's top over
    the chest, printed); other presets length
    and width / height. JSON under `proportions`.
  - `thickness: {parts?}`: per visible part, the thinnest point straight
    through it along the inward normal (and where), the 5% and median
    thickness, and whether its surface passes through itself: a limb
    pinched to nothing, a wall too thin to print.
  - `measure: [["a", "b"], ["c"]]`: two parts are apart (closest-surface
    gap), touching, or overlapping (shared volume and penetration depth),
    plus per-axis box gaps and where an overlap is centred (so an intended joint, like a stay meeting a hub, tells apart from a collision); one name gives its height above the ground.
  - `section: {axis, at, parts?}`: cuts with the plane `axis = at` and
    reports per part the area, outlines, holes and thinnest wall as numbers;
    `image: true` adds the picture of the cut (y = plan, z = front, x = side;
    640x480 at `detail: "low"`, 800x600 at `"high"`). Shows hidden
    interiors no outside view can: wall thickness, handles poking inside.
  - `mass: {parts?, density?: {part: kg/m3}, default_density?}` (`parts`
    weighs one object of a scene, `*` globs, on its own footprint): per-part and total
    mass, centre of mass, the footprint, and whether it tips over (with the
    tilt it tolerates). Default density 1000. Contact is measured from
    the ground plane y = 0 when the model is sunk up to 1 cm into it (so a
    part sunk a few mm doesn't hide the other contacts), otherwise from its
    lowest point (a table on a 6 mm rug stands on its feet).
