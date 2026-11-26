# `garment`

- **`garment`** cuts clothing out of the surface of the closed part it is
  `on` (a `human` body, an sdf creature, or another garment for layers),
  so it fits any shape parameters and re-derives with them (`src/garment.rs`).
  A region field over the body's (welded) vertices is clipped along its
  zero line (even hems, not stair-stepped faces), offset along smooth
  normals by `offset` meters, relaxed by `ease` (0 skin-tight, 1 bridges
  hollows, up to 3 merges toes / fingers; never closer than `offset` over
  its own source point), flared at the border by `hem: {flare, length}`
  and, with `thickness` > 0 (default 0.003), closed into a shell (inner
  layer at `offset`, outer at `offset + thickness`, borders stitched): a
  manifold solid that passes `validate`. `thickness: 0` is one open sheet.
  - `region`: `zones` (human only: head, neck, torso, hips, arms, hands,
    legs, feet, from the skin weights), `arms` (none / cap / short / elbow /
    forearm / wrist, or `[from, to]` along shoulder 0 -> elbow 1 -> wrist 2),
    `legs` (none / short / knee / calf / ankle or `[from, to]`, hip joint 0
    -> knee 1 -> ankle 2), `neck` (crew default, `high`, `v` or the v's
    depth), `above` / `below` (meters or a joint with an offset,
    `"hip-0.02"`, `"knee_l+0.06"`), `planes` [{normal, point}] (keep the
    side the normal points to), `boxes` [{min, max}]. Everything given must
    hold; zones are a union. On a body without skin weights use the
    heights, planes and boxes.
  - `fit` (tight / regular / loose / baggy, or `skin` to follow every
    contour; presets pick one: tops, dress and trousers `regular`):
    cloth bridges hollows and hangs instead of reading as body paint
    (`src/garment/fit.rs`). The eased sheet is split by skin weights into
    trunk, arms and legs; each is cut into 2 cm stations along its axis
    (world y, or arc length along the limb joints), each station's
    cross-section replaced by its convex hull (from bust straight to belly,
    across the buttock cleft), widened by the ease allowance
    (circumference / 2 pi) and, with hang, never narrower than the station
    above (the shirt drops from the bust, trousers keep the seat / thigh
    width down to the knee, `regular` keeps 75% below it, `loose` is
    straight to the hem). Then dips bridge (an inflate-only smoothing that
    never shrinks convex shapes), a long bridging pass spans the trouser
    fork, the spacing is evened along the surface, and nothing comes closer
    than `offset`. Ease allowances are standard pattern-making body
    allowances (Aldrich, Armstrong): `regular` top +10 cm chest, +8 cm
    biceps; `regular` trousers +6 cm hip, +8 cm thigh; `loose` +18 / +12
    (tops), +10 / +10 (trousers); `baggy` +28 / +18 / +16; `tight` 0 (knit,
    bridges only the deepest hollows). Allowances scale with body height
    (a child's are smaller). Hems, cuffs and collars are a rolled edge (2.2x
    the cloth over the last 12 mm); tops' hems flare 12 mm so they stand
    off and clear a waistband (untucked over trousers and skirts); soft
    folds ring the elbows and knees.
  - `preset` (on a `human`; every field overrides it, region fields one by
    one): `tshirt`, `longsleeve`, `tank`, `trousers`, `shorts`, `skirt`,
    `dress`, `socks`, shoes (`sneakers` = `shoes`, `boots`, `flats`; see
    below), `gloves`, `hood`, `briefs`, `bra`, and hair caps `hair_buzz`,
    `hair_short`, `hair_bob` (a hairline from the head joint; solid caps,
    no strand cards yet). Tops sit over bottoms.
  - **Shoes** are built volumes (`src/garment/shoe.rs`), not cut skin: each
    foot is measured heel to toe in its own frame (ankle -> ball), an upper
    lofted through 30 rounded superellipse sections grown 9 mm round the
    foot (a toe box bridging every toe, at least 4.5 cm over the sole, the
    collar at the ankle rising 4.5 cm into a tongue over the instep), on a
    sole slab 5 mm wider (the tread edge), 2.4 cm at the forefoot and 3.2
    cm at the heel, lifted 12 mm at the toe (toe spring) with a heel
    rocker. `boots` add a shaft 15 cm up the shin, `flats` an 8 mm sole and
    a low vamp. The foot (and a boot's leg) inside is hidden; weights come
    from the nearest foot vertex with toe bones moved onto the ball and
    smoothed over the shoe, so it bends at the ball. Clearance checks see
    the foot inside the sole by design.
  - `skirt: {top, hem? | length?, profile?, folds?, flare?}` bridges the
    legs: a tube from the convex hull of the body's horizontal slices (arms
    left out), never narrower than the ring above. `length` mini / knee
    (default) / midi / maxi from the hip, knee and ankle joints;
    `profile` a_line (default: flares 5 cm + 9 cm per 55 cm of length, by
    t squared) or straight (1.5 cm); `folds` the depth of soft vertical
    folds (0.006 m, about one per 11 cm of hem, fading to 0 at the waist).
    A skirt on its own gets a 3 mm waistband. The `dress` is one garment:
    its bodice ends on a level line where the tube starts, and the tube's
    top ring takes the bodice's lower border (radii per angle) and blends
    into the fall over 10 cm.
  - **Hides the covered skin** (`hide_skin`, default true): body faces
    the garment covers at least 12 mm from its borders are dropped from
    the body's index list at the end of evaluation while the garment is
    visible (vertices kept, so skin weights, morphs and correctives keep
    their counts; the masked body is marked a sheet so `validate` passes).
    The body op is unchanged: hide or delete the garment and the skin
    comes back, and a variant that hides it brings the skin back in that
    variant.
  - **Weights and bind**: on a body with skin weights the garment carries
    the body's own weights at every vertex (exact from the vertex it was
    cut from, diffused over its own edges). Where arm and trunk weights
    meet (the armpit, the shoulder seam) and 8 rings round it the weights
    keep diffusing (120 rounds): a gusset of smooth weights, so arm swing
    spreads over the junction. Every garment on a rigged `body` binds
    itself (`garment::implicit_binds`: `<name>_bind`, smooth, weights from
    the part it is `on`, correctives from the body); no `bind` item is
    needed. `bind: false` opts out; an explicit `bind` item of that name
    replaces it.
  - The default look is a built-in tile fabric per preset (`garment_<preset>`
    graphs: weave noise, mottling); any `material` item replaces it.
  - Measured (lib tests `garment::tests`, female / male / heavy / child):
    every preset builds in 2-30 ms (boots ~100 ms), 1.7k-17k triangles.
    The outfit example walk: worst edge stretch 1.95x (all garments, under
    2.0), an arms-up pose 2.06x; `verify` validate, weights, materials pbr
    (every variant) and loop pass. Known: the tank / dress bodice and
    briefs measure up to -1 cm on heavy / male bodies with the
    nearest-vertex metric at their cut borders; a few jeans waistband
    vertices show through the shirt's side seam in walk renders; the shoe
    toe cap is blunt.
  An outfit: `examples/human_outfit.json`.
