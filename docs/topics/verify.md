# verify

**Verify (deterministic; prefer these over eyeballing renders):**

- `verify(parts?, includes?, grounded?, supported?, rests?, collisions?, bounds?, validate?, rebuild?, stable?, materials?, self_intersections?, thickness?, coaxial?, flush?, centered_on?, symmetric?, equal_spacing?, density?, walkable?, scope?)` (all
  optional; no arguments runs validate + rebuild). Over visible parts only:
  - `scope: ["bars_*", "basket"]` limits what the other checks report to
    those parts (`*` globs): grounded / supported / rests / bounds /
    validate / stable look only at them (grounding still travels through
    every part, so a scoped part may rest on an unscoped one), and
    collisions keep pairs with at least one side in scope. `parts` stays an
    exact check of the whole model.
  - `parts`: exact part names, order-insensitive; reports missing and
    unexpected names. `*` matches any run of characters (`"gz.*"` covers a
    prefab instance).
  - `includes`: these names must exist; extras are fine (`*` works too).
  - `grounded`: every part reaches the ground through a chain of touching
    parts (no floaters; a door and its handle touching only each other in
    mid-air both float), by real geometry: shared volume, or surfaces within 0.2% of the smaller
    part's size (10 µm..1 mm), so a 0.8 mm gap under a laptop screen fails.
    (The `scene_summary` floating warning is a cheap bounding-box version.)
    A PASS also names parts **held up only by passing deep into another**
    (over 20% of the smaller part inside: "`bar` (through body)"), which
    usually means a collision is doing the holding; `grounded: "strict"`
    fails them.
  - `supported`: nothing hangs. Each part is on the ground or is held,
    through a chain of shared volume (a joint) or contact BELOW its centre
    of mass, by parts that are themselves supported: a pile that floats and
    props itself up fails. Catches a part only wedged against something
    above it, which `grounded` passes. The ground is y = 0; a model sunk up
    to 1 cm below it (grazing stems) still stands on it.
  - `rests: [["mug", "desk_top"], ...]`: each `a` rests on (or is
    attached to) `b`; failures say whether they don't touch at all or only
    touch at/above `a`'s centre of mass.
  - `attached: [["canvas_*", "arm_*"], ...]`: every part matching `a`
    touches or shares volume with a part matching `b`, on any side, with no
    gravity assumption (a canvas hung below a windmill arm, a wheel's
    spokes); use it instead of `rests` when `a` isn't sitting on `b`.
  - `bounds: {min, max, tol}`: per-axis world bounds, `null` skips an axis
    ("sits on the ground" = `min [null, 0, null]`); failures name the part
    that sets the extreme.
  - `validate`: every part closed and outward-wound.
  - `rebuild`: an uncached re-derivation matches the cache.
  - `stable`: the centre of mass (uniform density) sits over the footprint.
  - `materials`: `true` fails visible parts still on the default grey
    material; `"pbr"` also fails flat colours, so every part needs a
    procedural material (`material_ops`, then a `material` item with
    `pbr`). **Every model should pass `materials: true`**, and prefer
    `"pbr"` for anything that ships. Work shape first: build and check the
    geometry, then make the materials and assign them in a later
    `apply_ops` (a `pbr` item needs its material to exist), with the
    `materials` check in that batch's `expect`.
    `save_prefab` warns about grey parts (and notes flat colours).
  - `self_intersections: true`: no part's surface passes through itself
    (a deform, warp or skin that folded it); fails with the crossing count
    and a point.
  - `walkable: {"markers": {kinds?, names?, from?, agent?, cell?}}`: every
    spawn / waypoint / pickup (any marker but block / room / opening, or the
    given `kinds` / `names` globs) is standable and reachable from the first
    (or `from`): the one check for "can every spawn reach every pickup".
    A failure lists the offenders (not standable and why, floating or buried
    more than 0.5 m off the floor, not reachable) and draws a top-view map.
    A failed single route also draws that map ('+' reached, '.' standable but
    not reached, `S` start, `G` goal), and notes when a `from` / `to` y was
    snapped to a floor more than a step away (the wrong level). The ground
    is y = 0 unless the model sits wholly more than 5 cm lower (a prop sunk
    a few mm no longer grows every riser).
  - `walkable: {from, to, via?, agent?, cell?}` (or a list): a character
    can walk from `from` to `to` (feet points, snapped to the floor
    under them) over the visible parts and the ground. `agent` is
    `{radius 0.3, height 1.8, step 0.3, max_slope 45}` (a human player).
    The parts are rasterized into columns (`cell`, default radius / 4);
    a floor is standable when its slope is walkable and nothing taller
    than a step (or rising steeper than `max_slope`) sits within `radius`
    below `height`; neighbours join when they differ by at most `step`.
    A FAIL names up to four blockers nearest the goal (`step 0.40 m >
    0.30 up onto `stair_1``, `headroom 1.70 m < 1.80 under `landing``,
    `wall_l` in the way: a wall or a gap under 2 x radius, `slope 52°`)
    and the closest point reached; a PASS gives the route length, its
    tallest step and lowest headroom. `via` forces waypoints. Use it on
    stairs, doorways, corridors and level layouts.
  - **Relations** (boxes of the visible parts a name or `*` glob
    matches; each PASS/FAIL gives the measured offset; `tol` defaults to
    0.001 m): `coaxial: [["knob", "dial"]]` (or `{"pair": [a, b], "axis":
    "x", "tol"}`: centres agree on the two axes across `axis`, default y),
    `centered_on: [["vase", "table"]]` (footprint centre x/z on the other's),
    `flush: [{"a": "lid", "b": "box", "face": "top", "tol"?}]` (that face of
    both in one plane), `symmetric: {"parts"?: [...], "axis": "x",
    "center"?: 0, "tol"?}` (every part has a mirror twin; fails naming the
    ones without and how far off), `equal_spacing: {"parts": "post_*",
    "tol"?}` (centres on a line, gaps equal; names the worst gap).
  - `density: {min_features_per_m2?: 0.25, min_area?: 4}`: "sparse" scenes.
    Every flat visible surface (coplanar triangles, undersides skipped)
    over `min_area` m² needs that many features per m²: other parts
    touching or standing on it, insets / relief of its own inside it, or
    decals. Fails naming the part, the face's area and centre and whether
    it wears a flat colour, with hints (scatter, panels, decals, macro
    variation in the material).
  - `thickness: 0.004`: every part is at least that many meters thick
    everywhere (sampled straight through along the inward normal); fails
    naming the part and where.
  - `collisions`: no part passes through another. `true`, or `{allow:
    [["c_leg*", "c_seat"]], max: 0.2}`. Fails any pair where more than
    `max` (default 20%) of the smaller part's volume is inside the other,
    unless `allow` lists it (`*` patterns, either order). Shallow overlaps
    (a tap sunk 1 cm into a counter) pass and are listed as joints.
    `grounded`/`supported` count any overlap as a joint, so only this
    catches a part swung or placed into another. A failing pair says why
    when the model knows: `[x is an input of y and still visible: hide it]`
    (a blank, cutter or array source left showing) or `[entirely hidden
    inside]`. Intended embeds (a chimney through a roof, a seat post in its
    tube, a bulb in a shade) go in `allow`. Every overlap reports its
    **depth** (penetration along the axis it covers least of the smaller
    part). An allow entry is `[a, b]`, `[a, b, depth]` or `{"pair": [a, b], "depth": 0.003,
    "max": 0.5}`: excused only while no deeper than 3 mm / 50%. The PASS
    lists what each allow excused and its deepest pair, and an allow with a
    bare `*` side (`["bars_*", "*"]`) that hides an overlap over `max` gets a
    `note:` to name the real partner: a catch-all once hid typebars sunk
    into a typewriter body.

  Each check prints PASS/FAIL; structured clients get `{ok, checks}`.
