# AGENTS.md

modeler is a standalone mesh-modeling tool driven by an **MCP server on stdio**
with a live **Bevy preview window**. Agents drive it over MCP; humans watch and
annotate in the window. No game code.

```sh
cargo run                # MCP server + preview window (what the MCP config runs)
cargo run -- --headless  # no window/render — for piping JSON-RPC lines in tests
cargo run --example procedural_flower   # reference for the op-graph workflow
cargo test               # lib tests + gltf round-trip tests (fast loop)
cargo clippy --all-targets -- -D warnings && cargo fmt
```

This file is the long-form reference. The critical operating rules are also
served in the MCP `initialize` response (`instructions`), so MCP-connected
agents get them in context automatically; keep the two in sync when semantics
change.

## Getting started (agent)

```sh
cargo build                # or skip if target/debug/modeler is current
python3 mcp_driver.py --daemon --windowed &   # one persistent server + preview
```

Then talk to it with `python3 mcp_driver.py --connect --calls <file>` (JSON
array of `{"name", "args"}`), one batch per step, and `{"name": "quit"}` when
done. If the daemon's server has died (killed, crashed), the next call
restarts it from the autosave and says so; with no daemon at all the
client prints the start command. Typical first calls: `scene_summary` (also shows the autosaved session,
if any), `catalog_list`, then `new_model` + `apply_ops` to build. If your
session's MCP tool list has no `mcp__modeler__*` tools, this driver is the
documented fallback — it speaks the same tool surface over stdio. Each
result line is `{"ok", "text", "json"?, ...}`; `--json` drops the prose when
structured JSON is present. The daemon keeps the preview window open; check
it with `agent_views` / `screenshot` and verify numerically with `verify`.

## Units

**1 unit = 1 meter, always.** Y is up and the ground is y = 0. Size things
at real-world scale: a door is about 0.9 x 2.1, a table top sits at 0.75, a
mug is about 0.1 tall. glTF uses meters too, so exports need no scaling.

## The mental model

Three layers, read top to bottom:

1. **Op graph** (`src/graph.rs`) — how geometry *derives*. An ordered list of
   `Op` steps evaluated top-to-bottom into named parts (`Model.ops` →
   `Model.parts`, which is a cache; `refresh()` re-derives).
2. **History tree** (`src/lib.rs`, `History`/`Snapshot`) — how edits *remember*.
   Every mutation records a full-model snapshot as a child of the cursor;
   undo/redo/jump move the cursor O(1). Editing after a jump **branches** (the
   old future stays reachable as a sibling subtree). Nothing is pruned
   automatically.
3. **Prefab catalog** (`src/catalog.rs`) — how work *persists*. JSON files in
   `~/Library/Application Support/friendslop/modeler/prefabs` + a
   `catalog.json` index. Saves are non-destructive and atomic.

## MCP tool surface (32 tools)

**Build: one tool, `apply_ops`.** Everything that changes geometry is an
item in `apply_ops(ops, params?, expect?)`, each tagged with a `"type"`.
`describe_op(kind?)` returns an example payload and the JSON schema per
type, so there is no need to memorise field names.

| type | fields (optional in []) |
|---|---|
| `primitive` | name, kind, size, [translation / position], [rotation quat] |
| `extrude` | name, profile, height, [twist], [scale_top [1,1]], [slices], [round], [translation], [rotation] |
| `revolve` | name, profile, [degrees 360], [segments 48], [round], [translation], [rotation] |
| `sweep` | name, profile (one ring), path [[x,y,z]...], [closed], [round] |
| `hull` | name, inputs |
| `round` | name, input, mode (fillet / chamfer / smooth), [radius], [segments] |
| `shell` | name, input, thickness, [open: face or [faces]] (hollow a solid) |
| `group` | name, inputs |
| `transform` | name, input, [translation], [rotation], [scale], [relative], [pivot] |
| `move` | [name], parts (names / `*` globs; `["*"]` = all), [translation], [rotation] or [axis + degrees], [pivot: point or anchor] |
| `place` | [name], parts, on, [at], [gap], [toward down] (drop onto `on`, or mount against a wall) |
| `boolean` | name, op (union / difference / intersection), a, b |
| `radial_array` | name, input, count, [center], [degrees 360], [start_degrees], [axis y], [include_source true] |
| `linear_array` | name, input, count, offset |
| `mirror` | name, input, axis, [offset] |
| `material` | name, [input = name], [color / base_color], [roughness], [metalness] |
| `prefab` | name, prefab (catalog entry), [translation], [rotation], [scale] |
| `hide_part` / `show_part` | part (`*` matches anything: `"cut*"` hides every cutter, `"*_raw"` every raw blank) |
| `rename` | part, to |
| `delete` | name (an op output, or a baked part; its own in-place hide/material/transform edits go with it) |
| `tri_mesh` | name, verts, indices, [normals], [uvs] (baked raw triangles) |
| `uvs` | part, projection (box / planar / cylindrical / spherical / metric = box in meters) |

A `material` item may also carry `pbr` (a procedural material by name, see
**Procedural materials** below), with `params` overrides and `scale`.

- **Ops are upserts**: re-issuing an item with the same name *and type*
  replaces it and re-derives (the tweak path). Evaluation is
  dependency-ordered, and a batch runs an item that reads a part made later
  in the same batch after its producer, and edits naming parts (hide/show,
  rename, uvs, move/place) run after every op in the order written, so
  `"*_raw"` sees every part the batch makes and a later `show_part` still
  wins: item order doesn't matter.
- **Parsing is strict but forgiving.** All items are parsed before anything
  is applied; errors name the item and field
  (`op #3 (primitive "roof"): size: cone size is [radius, height]...`), and
  unknown fields (typos) are rejected. Accepted forms:
  - `size`: a number (sphere/disc radius; box/wedge cube edge), a short array
    (`[radius]`, `[radius, height]`, `[ring, tube]`, `[inner, outer]`, box
    `[x, y, z]`), or an object (`{radius, height}`, `{width, height, depth}`).
    A sphere is round: flatten it with a transform's scale.
  - colours: `[r, g, b]`, `[r, g, b, a]`, 0..1 or 0..255, or `"#rgb"` or `"#rrggbb(aa)"`.
- **Materials follow lineage.** An in-place material on X runs before every
  op that copies or cuts X, wherever it sits in the batch, so arrays, mirrors
  and transforms of X inherit the colour.
- **Transforms** are absolute by default; `relative: true` makes them offsets
  from the input (translation adds, rotation composes, scale multiplies).
  With `pivot`, a relative rotation/scale turns about that world point
  instead of the part's own origin.
- **`move`** is a parent transform (one level of hierarchy): every part
  matching `parts` moves as one rigid body, rotated about `pivot` (default:
  the centre of their bottom face, fixed when issued) and then translated.
  It applies after the whole graph is evaluated, so no op ever reads the
  moved result: mirrors, arrays and cuts are built from the unmoved parts and
  carried along, and the model's recipe stays readable. Several moves compose
  in list order (later = outer parent). Give it a `name` and re-issue that
  name to adjust, e.g. lean a bike onto its kickstand with
  `{"type": "move", "name": "lean", "parts": ["*"], "axis": [1, 0, 0], "degrees": 8, "pivot": [0, 0, 0]}`.
  Every move/place result says where the moved parts' centre went
  ("0.25 m front (+z)": a door swinging the wrong way shows here) and
  warns about parts **left behind** (they touched a moved part and now
  touch nothing: a handle not listed with its door) and **new overlaps**
  with unmoved parts (the door swinging into its cabinet).
- **`place`** drops parts onto a surface so nothing is placed by hand
  arithmetic: `{"type": "place", "name": "mug_on", "parts": ["mug"], "on":
  "desk_top", "at": [-0.4, 0.1]}` slides the mug's footprint centre to
  x -0.4, z 0.1, then moves it vertically until its underside rests on the
  desk's top surface (sloped and uneven tops work; `gap` leaves clearance).
  It is a named, post-graph move like `move`, so it re-runs on every change:
  lower the desk with a param and the mug follows. `toward` sets the travel
  direction: `down` (default), `up` (under a shelf), or `back` / `front` /
  `left` / `right` to mount against a wall; `at` then names the two cross
  axes: back/front `[x, y]`, left/right `[z, y]`. A frame on the left wall:
  `{"type": "place", "name": "hang", "parts": ["frame"], "on": "wall_left",
  "toward": "left", "at": [-0.4, 1.5]}`. A place runs after any move/place
  that moves the part it lands on (a pot on a sill mounted on a wall),
  whatever order they are written in. List every part that should move
  (`"bracket*"` takes the array copies too).
- **Anchor pivots**: a move's `pivot` may be a point or an anchor on a
  bounding box: `"bottom-back"` (the moved parts' own box) or
  `"laptop_screen:bottom-front"` (another part's box; `*` patterns work).
  Words: left/right (x), bottom/top (y), back/front (z, front = +z),
  center; axes not named stay centred. Anchors and the default pivot are
  measured on the parts *without* this move, so re-issuing a named move
  never drifts. A laptop hinge: `{"type": "move", "name": "hinge", "parts":
  ["screen"], "axis": [1, 0, 0], "degrees": -15, "pivot": "screen:bottom-front"}`.
- **Radial arrays** default to `include_source: true`: the source is item 0
  and `count` items spread evenly (copies `name_1..`). With `false`, `count`
  new copies `name_0..` start at `start_degrees` and the source stays put.
  `axis` "x"/"z" makes horizontal rings (windmill blades).
- **Linear arrays**: the source is copy 0; outputs `name_1..{count-1}`.
- **Extrude / revolve** build closed solids from 2D profiles (Manifold),
  ready for booleans. `profile` is one ring `[[a, b], ...]` or a list of
  rings; inner rings are holes, orientation doesn't matter (even-odd).
  - `extrude`: footprint points are `[x, z]` on the ground, extruded up
    y = 0..height. `twist` (degrees) and `scale_top` act about the profile
    origin `[0, 0]`, so centre the footprint there for a symmetric taper
    (`scale_top: [0, 0]` = pyramid/spire). A flat cut-out shape (flag, sign,
    gear) is a thin extrude stood up with rotation `[-0.707, 0, 0, 0.707]`
    (footprint z becomes height, thickness goes to -z).
  - `revolve`: points are `[radius, y]` (radius >= 0) around the y axis:
    vases, bowls, columns, bottles. `degrees` < 360 leaves flat end caps.
    Hollow a vessel by revolving an inner profile and taking a `difference`.
- **Profiles** (extrude/revolve/sweep): ring entries are points or arcs,
  `{"arc": {"center": [a, b], "radius": r, "from": deg, "to": deg}}`
  (counter-clockwise from +a; 0 -> 360 is a full circle), so curves need no
  hand-made point lists. `round: r` fillets every profile corner.
- **`sweep`** runs a closed profile along a 3D polyline (pipes, rails,
  handles, frames, hoses); bends are mitred to keep the thickness, and
  `closed: true` loops the path (a ring) instead of capping it. Profile u is
  along the frame normal (world up to start, or +z on a vertical path).
- **`hull`**: the convex hull of several parts. Two spheres make a pill,
  eight small spheres at a box's corners make a rounded box.
- **`shell`** hollows a closed solid: every face moves in by exactly
  `thickness` (erosion by a ball, so convex corners stay sharp), and each
  face named in `open` (top/bottom/left/right/front/back, world axes) is left
  open. A cabinet carcass: `{"type": "shell", "name": "cab", "input":
  "cab_box", "thickness": 0.018, "open": ["front", "top"]}`; a planter or
  well: `open: "top"`; a ring or band: `open: ["top", "bottom"]`. Hide the
  input. Doors over an open front should overlap the side panels, or they
  touch only air and fail `grounded`.
- **`round`** works on any closed solid:
  - `fillet` rounds convex edges by `radius` and keeps the outer size
    (Minkowski opening).
  - `chamfer` is a 45° bevel on axis-aligned edges.
  - `smooth` does subdivision rounding.

  Large or concave inputs make fillet/chamfer slower, so round parts before
  combining them.
- **`group`** merges parts into one mesh without a boolean (they may be
  disjoint), so a set of parts can be arrayed or moved as one. Use it
  instead of chained unions: a union of separate parts warns "no overlap".
  It takes the first input's material.
- **Mirror** is baked (verts reflected, winding flipped); never mirror with
  negative scale.
- **Booleans** run on Manifold (the kernel OpenSCAD and Blender use): outputs
  are closed solids, coplanar and curved-on-curved cuts are exact, and input
  UVs ride through; normals are creased at 40°. **Both inputs must be closed
  solids**: disc/plane/washer (open sheets) error with "is not a closed
  solid". A failed op rolls back the whole call; the graph never stays broken.
- **disc/washer/plane** are double-sided sheets in the XY plane (vertical);
  lay one flat with rotation `[-0.707, 0, 0, 0.707]`.
- **Params**: `params: {"tower_h": 2}` sets named numbers before the items
  run; any value may be `"$tower_h"` or `"=$tower_h / 2 + 0.1"`. The model
  remembers which ops read which params, so a later
  `apply_ops({"params": {"tower_h": 3}})` re-derives all of them in one
  history step.
- **Prefabs as parts**: `{"type": "prefab", "name": "lamp1", "prefab": "lamp"}`
  instances a saved catalog model; its visible parts become `lamp1.pole`,
  `lamp1.shade`, ... and feed later ops like any part. The part list is
  captured when the item is applied; re-issue it after re-saving the prefab.
- **Batches are atomic**: if any item fails, the whole call rolls back
  (the error says so) and the model is exactly as before. Items may read
  parts made later in the same batch.
- **`expect`** runs checks after the batch in the same call (fields as for
  `verify`); any failure makes the call an error, but the batch stays applied.
- Part lists fold array runs: `+book, +book_1..13`.
- The result lists only notable items (routine `ok` lines become
  "applied N item(s)") and ends with a **brief summary**: part/op counts, bounds, the
  parts this call added (`+`), changed (`~`) and removed (`-`), and every
  warning. Pass `summary: "full"` (or call `scene_summary`) for the whole
  part list; structured clients always get the full summary as JSON
  (`structuredContent`).
- **Failures explain themselves**: an empty boolean names the axis and gap
  separating the inputs (or says the cutter swallows `a`, maybe swapped); a
  non-solid input reports its open-edge count; `round` refuses inputs over
  20k triangles and says to round pieces first, and a too-large radius
  reports the part's size.

Inspect: `scene_summary` (call FIRST and after anything confusing: parts,
bounds, ops, params, annotations and **warnings**), `ops` (the graph, one
line per op with full parameters) and `op_get(name)` (every op writing that
name: the producer first, then in-place material/visibility/transform edits).

**Verify (deterministic; prefer these over eyeballing renders):**

- `verify(parts?, includes?, grounded?, supported?, rests?, collisions?, bounds?, validate?, rebuild?, stable?)` (all
  optional; no arguments runs validate + rebuild). Over visible parts only:
  - `parts`: exact part names, order-insensitive; reports missing and
    unexpected names. `*` matches any run of characters (`"gz.*"` covers a
    prefab instance).
  - `includes`: these names must exist; extras are fine (`*` works too).
  - `grounded`: every part reaches the ground through a chain of touching
    parts (no floaters; a door and its handle touching only each other in
    mid-air both float), by real geometry: shared volume, or surfaces within 0.2% of the smaller
    part's size (10 µm..1 mm), so a 0.8 mm gap under a laptop screen fails.
    (The `scene_summary` floating warning is a cheap bounding-box version.)
  - `supported`: nothing hangs. Each part is on the ground, shares volume
    with another (a joint), or touches another BELOW its centre of mass.
    Catches a part only wedged against something above it, which
    `grounded` passes.
  - `rests: [["mug", "desk_top"], ...]`: each `a` rests on (or is
    attached to) `b`; failures say whether they don't touch at all or only
    touch at/above `a`'s centre of mass.
  - `bounds: {min, max, tol}`: per-axis world bounds, `null` skips an axis
    ("sits on the ground" = `min [null, 0, null]`); failures name the part
    that sets the extreme.
  - `validate`: every part closed and outward-wound.
  - `rebuild`: an uncached re-derivation matches the cache.
  - `stable`: the centre of mass (uniform density) sits over the footprint.
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
    tube, a bulb in a shade) go in `allow`.

  Each check prints PASS/FAIL; structured clients get `{ok, checks}`.
- `probe_ray(origin, dir, all_hits?)`: the first part hit (or `all_hits`:
  every part, nearest first). Proves openings are open and fills are solid.
  Runs on the model itself, so it works headless too.
- `agent_views(views, width?, height?, focus?, grid?)`: offscreen renders,
  independent of the human's window.
  - Up to 9 views: `front`/`back`/`left`/`right`/`top`/`bottom`
    (orthographic, auto-framed), `iso` (perspective), or a camera you place,
    `{yaw, pitch, dist?, target?, ortho?, fov?}` (yaw 0 = from +z/front,
    90 = from +x/right).
  - Several views come back tiled into ONE labelled image (`grid: false` for
    separate images), so `["front", "right", "top", "iso"]` is one read.
  - The top view is a plan: +x right, -z up.
  - `focus: [parts]` frames just those (`*` globs work: `"bike.*"`), and framing works at any scale.
  - A shadowed key light makes openings read even in orthographic views.
  - Both the window and agent cameras are lit by a procedural photo
    studio (`src/material/env.rs`: soft boxes over a grey cyclorama, filtered
    on the GPU as an environment light), so glossy and metal parts show real
    reflections, and screen-space reflections (deferred rendering, MSAA
    off, SMAA instead) so glossy parts also mirror the parts around them.
    Agent views add SSAO contact shading; the window does not (SSAO froze
    its presentation on Metal, and so does a forward window camera over the
    deferred materials: keep the window camera deferred).
    `material_view` shades its sphere with the same studio. Renders wait for
    the environment light to finish filtering, so a shot right after startup
    is never blank.
  - Images are sized to stay under ~1.15 MP (long edge 1568), the size a
    model reads at full detail; bigger grids shrink their cells (the
    result says so). A 2x2 grid of 640x480 is ~1.6k tokens.
  - `style: "ids"`: each part a flat, unlit, distinct colour with a legend
    (and the colour order in the text), to tell which shape is which part.
  - `dims: true`: the framed box's width and height and a scale bar, at true
    scale in orthographic views. Perspective views have no single scale:
    they state the model's box size ("not the view") and a scale bar valid
    only at the look-at point's depth.
  - `diff: true`: compares with the previous render of the same view, size,
    focus and style: magenta where something now is, cyan where something
    was, the rest greyed, plus the changed share and pixel box.
- `inspect(measure?, section?, mass?)`: numeric senses, any combination per
  call.
  - `measure: [["a", "b"], ["c"]]`: two parts are apart (closest-surface
    gap), touching, or overlapping (shared volume and penetration depth),
    plus per-axis box gaps and where an overlap is centred (so an intended joint, like a stay meeting a hub, tells apart from a collision); one name gives its height above the ground.
  - `section: {axis, at, parts?}`: cuts with the plane `axis = at` and
    reports per part the area, outlines, holes and thinnest wall, plus an
    image of the cut (y = plan, z = front, x = side). Shows hidden
    interiors no outside view can: wall thickness, handles poking inside.
  - `mass: {parts?, density?: {part: kg/m3}, default_density?}` (`parts`
    weighs one object of a scene, `*` globs, on its own footprint): per-part and total
    mass, centre of mass, the footprint, and whether it tips over (with the
    tilt it tolerates). Default density 1000. Contact is measured from
    the ground plane y = 0 when the model is sunk up to 1 cm into it (so a
    part sunk a few mm doesn't hide the other contacts), otherwise from its
    lowest point (a table on a 6 mm rug stands on its feet).
- `screenshot` (the human's view; aim with `set_camera(yaw, pitch, dist)`,
  same yaw convention as agent cameras).
  Render tools time out after 15 s instead of hanging. **Always look at an
  image after building**: the data can be right while the render is wrong.

**History:**

- `undo` / `redo` / `jump(id)` / `history(limit?)` / `history_prune(keep)`:
  the edit tree. Each `apply_ops` (or `cherry_pick`) call is ONE node, so
  one undo reverts the whole call. `history` shows the newest 30 nodes plus
  the cursor by default. Editing after a jump branches.
- `diff(from, to?)`: ops added (+), removed (-) and changed (~, with the old
  parameters) between two nodes (default `to`: the live model), plus param
  changes.
- `cherry_pick(from, ops?)`: replay ops from another node onto the live model
  (all new or changed ops and params, or just the named ops/params). A
  picked op brings the param values it was built with, so the graph stays
  consistent. After a jump, this replaces re-issuing the old branch by hand.

**Model, catalog, output:** `new_model(name)` (the old model stays one undo
away), `set_model_name(name)` (the prefab entry name), `save_prefab(description,
tags, prompt)` (all required; reports when it overwrote an entry),
`load_prefab(name)` (answers with a brief: counts, visible parts, params,
named moves, warnings), `catalog_list` and `catalog_describe` (each entry also
lists its knobs: params with values and named moves/places, read from the
file),
`export_model(path, "glb" | "gltf")` (skips hidden parts).

**Annotations:** `add_annotation(part, text, face?, vertex?, position?)`
(omit the optionals to consume a spot the human clicked), `list_annotations`,
`delete_annotation(id)`.

**Session:** `session_log(limit?)` is the per-call record (tool, ok/error,
ms). Every mutation autosaves the whole session (parts, ops, params, history,
annotations) to `session.json`, restored on startup, including the earlier
items of a batch that failed partway. `save_prefab` only names and indexes it.

## Procedural materials

Materials are node graphs (SDFs, noise, maths, colour) evaluated on the CPU
in parallel and baked into PBR textures. Same app, same server, same
structures as models: nodes are upserts by name, batches are atomic, params
work, and every material has its own history tree.

- `material_ops(material, ops, params?, domain?, tile?, resolution?, expect?)`
  edits (or creates) a working material. Each node is `{"type", "name",
  ...}`; `{"type": "delete", "name"}` removes one. Inputs are numbers,
  `[r, g, b]`, `"#rrggbb"` (sRGB, stored linear) or another node by name
  (`"bricks"`, or a port: `"bricks.id"`). Coordinate inputs (`coords`)
  default to the sample's uv in meters, so warping is "transform the coords,
  feed them on". `describe_op("nodes")` lists every node type with an
  example; `describe_op("node:<type>")` gives one schema.
- **Domains.** `tile` (default): a seamless square `tile` meters wide.
  Noise lattices snap to a whole number of cells per tile so it repeats
  exactly; on parts it is mapped by metric uvs, so a 0.25 m brick is 0.25 m
  everywhere. `object`: evaluated at points on the surface (`input` of
  `position` / `normal`, object space, meters): 3D noise and SDFs run through
  the solid and wrap edges with no seams. Each part gets a packed atlas
  (box-unwrapped islands at one texel density, dilated gutter).
- **Node families.** Coords: input, transform, combine, split, polar,
  kaleido, warp, tile_grid (ports local / cell / id / edge), repeat,
  triplanar. 2D SDFs: circle, rect, segment, polygon, star, hexagon, arc,
  rings. 3D SDFs: sphere, box, torus, capsule, cylinder, plane. SDF ops:
  sdf_op (union / intersect / subtract and smooth_ versions), sdf_round,
  onion, displace, mask (fill / stroke / glow, anti-aliased). Noise: noise
  (value / gradient; fbm / ridged / turbulence), worley (f1 / f2 / edge /
  id). Maths and colour: math, remap, random (per id), color, ramp, hsv,
  blend, levels. Neighbourhood: blur, normal_from_height, ao_from_height,
  curvature, dilate.
- **`output`** names the channels: base_color, roughness, metallic,
  height (meters; the normal is derived from it, times normal_strength,
  unless `normal` is given), ao, emissive, opacity. Missing channels use
  defaults. Normals are in the glTF convention (+y up the image).
- **`expect`** checks: `tileable` (bakes the tile shifted by half a tile and
  compares: a non-periodic node fails, naming the channel), `ranges` (finite,
  0..1), `albedo` (non-metal luminance in 0.02..0.9 at the 1st / 99th
  percentile, or your range), `normals` (unit, facing out), `coverage`
  ([{node, min, max}]: share of the area where that node is above 0.5).
- `material_view(material?, node?, channels?, size?, diameter?)`: one
  image, rendered on the CPU from the bake that gets exported (works
  headless): a lit sphere plus channel thumbnails, or one node's output
  (`"g.id"`), with the value range in the text.
- `material_catalog(action, ...)`: `list` (the library), `working` (this
  session's materials), `describe`, `save` (name, description, tags,
  prompt), `load`, `delete`. The library is `materials/` next to the
  prefabs, shared by every model. Working materials autosave apart from the
  model (`materials/session.json`).
- **On parts:** `{"type": "material", "name": "wall", "pbr": "bricks",
  "params": {"mortar": 0.012}, "scale": 1.5}`. The graph is embedded in
  the op, so models and prefabs bake identically without the library (re-issue
  the item after changing the material). Materials follow lineage like
  colours; the uv mapping runs after the whole graph, so cut faces and copies
  are mapped too (it replaces any `uvs` projection on those parts). Factors
  become 1 (white) unless given, then multiply the textures.
- **Export:** `export_model` writes the baked textures (PNG in the buffer:
  base colour, metallic-roughness, normal, occlusion / emissive when driven;
  shared per bake) and a TANGENT attribute.
- **History:** `undo` / `redo` / `jump` / `history` take `doc:
  "material:<name>"` (or `"material"` for the active one).
- **Parallelism:** a level's nodes run in parallel, every kernel is
  data-parallel over samples (rayon), results are deterministic at any
  thread count, and a memo cache keyed by node + input keys + domain
  re-runs only what a tweak touches (results report "N computed").

## Semantics worth knowing

- **In-place ops**: a material, transform or visibility op whose output is
  its own input is legal; upsert never replaces an op the new op reads from.
  Repeated in-place ops accumulate; last wins.
- **Op results are memoized** (`graph::evaluate`): a process-wide cache keyed
  by op parameters + input content hashes, holding shared (`Arc`) parts, so a
  tweak re-runs only the op and its dependents. `verify` with `rebuild`
  bypasses it.
- **Op evaluation is dependency-ordered.** Each op runs after the op producing
  its inputs (and, for material lineage, after in-place materials on them).
  A true cycle fails with "ops form a dependency cycle involving ...". Ops
  can only reference op outputs, never baked parts.
- **Edits to op-derived parts are graph ops**: hide/show, rename, in-place
  material and in-place absolute transform on a part an op produces are
  recorded as ops, so they survive re-derivation. On baked parts (from
  `tri_mesh`) they edit the part directly; `uvs` is always baked.
- **Op-driven snapshots store no parts**; undo/redo/jump re-derive via the
  eval cache. Snapshots are history-less (the live tree is the single source
  of truth).
- **Jump restores the snapshot's op list exactly**; use `diff` to see what
  the other branch had and `cherry_pick` to bring it over.
- **Save format is versioned** (`Model::format_version`, `FORMAT_VERSION`):
  old files read as 0 and load; files from a newer build are rejected.
  Fully op-derived prefabs save ops, params, templates, history and
  annotations without parts. Writes are `.tmp` + rename.

`scene_summary` warnings: full-ring radial arrays whose copy 0 overlaps the
source (only with `include_source: false`), visible parts still wearing the
default material, parts with identical world bounds (z-fighting; when one
is an input of the op making the other, the warning names it and says to
hide it), boolean
volume sanity (a difference that didn't shrink its input = the cutter
misses; a union whose volume exactly sums its inputs = no overlap, unless the union is a cutter fed to a difference) and
boolean outputs with open edges enclosing real area (a safety net; Manifold
outputs are closed), and **floating parts** (off the ground and touching no
other part, e.g. a copy transformed instead of its neighbour). Volume checks
and welds scale with the part, so centimetre-scale models validate
correctly. Hidden parts are excluded. An empty list means none
fired.

## Preview window (human side)

Left/middle/right-drag = orbit/pan (pan follows the cursor, with a floor so
large scenes stay quick to pan when zoomed in), scroll = zoom (trackpad pixel
scroll is scaled to wheel lines), arrows + PageUp/Down too.
Auto-drift stops once the human steers; a model change re-frames the camera
around true world bounds (bounds include part transforms). **G** toggles the
node-graph pane (left half; the 3D view shrinks to the right half, and `screenshot`
captures only the 3D pane, never the graph; `MODELER_GRAPH=1` opens it at start). It has one box per
geometry op, coloured by kind, with edges from each input to its reader. In-place
edits show as `+color` / `+hidden` / `+placed` / `+renamed` / `+moved` badges, and
a whole-model `move` shows in the footer. Boxes whose output never shows are dimmed. A
spring layout lays it out: new or changed ops drop in and glow. Wheel zooms, any drag
pans, clicking a box makes its parts pulse in 3D and double-clicking frames them (`src/graph_view.rs` holds
the view and layout; the panel lives in `preview/panel.rs`). **M** toggles the material pane in the same left half (G and M replace each other;
`MODELER_MATERIALS=1` opens it at start): the active material's graph by dependency level with a live
thumbnail per node, a lit sphere and the channels, and clicking a node shows its
output large (click again for the material). Click a part then type to annotate
(Enter commits, Esc cancels; Backspace deletes; spaces work).

## Dev notes / gotchas

- Agents driving the server over stdio (frozen MCP tool list) should run
  `python3 mcp_driver.py --daemon --windowed` once in the background and then
  `python3 mcp_driver.py --connect --calls <file>` per batch: one long-lived
  server means the preview window stays open and state is live between
  invocations (autosave covers even a daemon restart). `{"name": "quit"}`
  stops it. The default throwaway mode spawns and kills a server per
  invocation — window flashes open/closed, state restored from autosave.

- `MODELER_STDERR=<file>` on the daemon keeps the server's stderr (panics,
  bake errors); it is discarded otherwise.
- rmcp 1.8 server runs a tokio current-thread on a spawned thread; the Bevy
  app owns the main thread (winit on macOS). Bridge = tokio mpsc + oneshot.
- `--headless` uses MinimalPlugins — don't add ScheduleRunnerPlugin again.
- Lints: `clippy::unwrap_used` and `expect_err` are denied — use
  `let Ok(..) = .. else`, `unwrap_or_else(|e| panic!("{e}"))` in tests, and
  return ids from APIs instead of unwrapping options.
- `bevy::shape` (not `bevy::math::primitives`) is where `Circle`/`Annulus`
  live; `Annulus` fields are `inner_circle`/`outer_circle`.
- f32: never assert derived coordinates with `round()` (0.49999997 rounds to
  0) — compare with a 1e-5 tolerance.
- clippy denies `chunks_exact(3)` with a const chunk — use
  `as_chunks::<3>().0`.
- In tests that hit the catalog, set `FRIENDSLOP_DATA_DIR` to a tempdir (the
  IO is funneled through `catalog_dir()` for exactly this).
- Bevy 0.20: `MeshRayCast` borrows `Assets<Meshes>` (defer pin spawns);
  `AccumulatedMouseScroll` for the wheel; winit sends `Key::Space`, not a
  space character.
- **Restart flow** (after code changes): `cargo build`, then
  `pkill -f target/debug/modeler` — the MCP client respawns the server from
  the binary on disk on the next tool call. The respawned session AUTOSAVES
  and RESTORES: every successful mutation writes the whole session model
  (parts, ops, history tree, annotations) to `session.json` in the catalog
  dir, and a server without an explicit `--model` restores it on startup, so
  unnamed WIP survives restarts. `new_model` starts clean (the old model
  stays one undo away, also in the autosave). `--model <path>` skips the
  restore and loads that file instead. The MCP tool list is fixed per
  connection, so newly added tools only appear in a NEW session.

## Conventions

- New ops: add the variant in `lib.rs` (`Op`), an eval arm + `output`/`kind`/
  `inputs`/`describe` arms in `graph.rs`, and in `src/spec.rs` a `*Spec`
  struct (`deny_unknown_fields`), an `OpSpec` variant, an `OP_TYPES` entry,
  a `parse_item` arm, an `example` and a `describe_type` schema arm (the
  `every_type_has_a_parseable_example` test enforces the last two). No new
  MCP tool: `apply_ops` carries it. Validate op params BEFORE the input
  lookup so errors report the right cause.
- Keep the tool count small: every tool's schema costs context on every
  turn. New capabilities go into `apply_ops` item types or `verify` fields.
- Examples in `examples/` double as executable documentation of the intended
  workflow — and as the fallback for authoring procedural prefabs when the
  agent's MCP tool list predates a tool.
- Keep `README.md` for humans, this file for agents.
