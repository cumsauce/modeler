# AGENTS.md

modeler is a standalone mesh-modeling tool driven by an **MCP server on stdio**
with a live **Bevy preview window**. Agents drive it over MCP; humans watch and
annotate in the window. No game code.

```sh
cargo run                # MCP server + preview window (what the MCP config runs)
cargo run -- --headless  # no window/render — for piping JSON-RPC lines in tests
cargo run --example procedural_flower   # reference for the op-graph workflow
cargo test               # lib tests + gltf round-trip tests (fast loop)
cargo test --test perf -- --nocapture   # timing budgets (edit, place, verify, autosave)
cargo clippy --all-targets -- -D warnings && cargo fmt
```

This file is the long-form reference. The critical operating rules are also
served in the MCP `initialize` response (`instructions`), so MCP-connected
agents get them in context automatically; keep the two in sync when semantics
change.

## Getting started (agent)

**One server, one window for every session.** The MCP config runs
`python3 mcp_driver.py --proxy`, not the binary: each Claude session gets a
thin stdio proxy, and all proxies share one daemon (started windowed on
first use, detached, so it outlives the sessions). The `mcp__modeler__*`
tools and the driver below therefore talk to the same server and the same
preview window. Each session is its own client (per-client active
documents, authorship, undo guards). Closing the window ends the server;
the next call restarts it from autosave. The shared library uses
`/tmp/modeler-mcp.sock`; a game project gets its own daemon and socket
(`/tmp/modeler-mcp-<hash>.sock`, server working in that project), since a
server works in one project at a time. After `cargo build`, restart the
server with `pkill -f target/debug/modeler` (the daemon respawns it on the
next call) or stop everything with a `quit` call.

Without MCP tools (or for scripted batches), use the driver directly:

```sh
cargo build                # or skip if target/debug/modeler is current
python3 mcp_driver.py --daemon --windowed &   # skip if the shared daemon is up
```

Without `--windowed` the daemon runs `--offscreen`: the full renderer with no
window. `agent_views` works, and `screenshot` renders the window camera's
view offscreen (1280x800; `set_camera` and auto-framing steer it as usual).
`--headless` drops rendering entirely.

Then talk to it with `python3 mcp_driver.py --connect --calls <file>` (JSON
array of `{"name", "args"}`), one batch per step, and `{"name": "quit"}` when
done. If the daemon's server has died (killed, crashed), the next call
restarts it from the autosave and says why it had exited ("code 0: window
closed"); with no daemon at all the client prints the start command, and a
second `--daemon` on the same socket refuses to start ("a modeler daemon is
already running"). `--save-shot PATH` names the run's first image PATH and
every later one PATH_2, PATH_3, ...; each result lists `saved_images`. Typical first calls: `scene_summary` (also shows the autosaved session,
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
3. **Prefab catalog** (`src/catalog.rs`) — how work *persists*. JSON files +
   a `catalog.json` index per kind (prefabs, styles, materials). Saves are
   non-destructive and atomic. **Where** (`catalog::kind_dir`):
   - **In a game project** (the default for real use): `<project>/.modeler/`,
     next to `assets/` and never inside it, so the editable sources (ops,
     history, styles) live in the game's repo but don't ship. The project is
     `--project <dir>` (or `MODELER_PROJECT`), else the nearest ancestor of
     the server's working directory with a `.modeler/` folder or a
     `Cargo.toml` beside an `assets/` folder. Relative `export_model` paths
     land in `<project>/assets/` (`"models/crate.glb"` ->
     `assets/models/crate.glb`). `.modeler/.gitignore` is written once to
     keep the per-machine autosaves (`session.json`, `open_docs.json`) out
     of git. `catalog_list`'s first line says which library is in use.
   - **Otherwise** the shared library
     `~/Library/Application Support/friendslop/modeler/` (this repo: no
     `assets/`, so testing uses the shared one). `FRIENDSLOP_DATA_DIR`
     overrides both (tests).

## MCP tool surface (34 tools)

**Build: one tool, `apply_ops`.** Everything that changes geometry is an
item in `apply_ops(ops, params?, expect?)`, each tagged with a `"type"`.
`describe_op(kind?)` returns an example payload and the JSON schema per
type, so there is no need to memorise field names.

| type | fields (optional in []) |
|---|---|
| `primitive` | name, kind, size, [translation / position], [rotation], [frame] |
| `extrude` | name, profile, height, [twist], [scale_top [1,1]], [slices], [round], [translation], [rotation] |
| `revolve` | name, profile, [degrees 360], [segments 48], [round], [translation], [rotation] |
| `sweep` | name, profile (one ring), path [[x,y,z]...], [closed], [round], [smooth], [samples], [twist], [scale_end] |
| `loft` | name, sections [{profile, y / at, [rotation], [scale]} or {profile, t}...], [path], [smooth], [samples], [steps], [round] |
| `lsystem` | name, preset (tree / pine / bush / fern / weed / roots) or axiom + rules, [iterations], [angle], [length], [radius], [seed], [tropism], [leaf_size], [leaf_mode card / solid], [leaf_cross], ... (branching growth: wood in `name`, leaves in `name_leaves`, cards for an alpha-cutout leaf material) |
| `text` | name, text, size (capital height), [depth size/8], [font sans / bold / mono / path.ttf], [align center], [line_spacing], [letter_spacing], [translation], [rotation] |
| `terrain` | name, [size [60, 60]], [resolution 0.5], [height 6], [scale 20], [octaves 5], [style hills / ridges / dunes / mesa / flat], [seed], [base -1], [flatten], [paths], [edge_falloff], [terraces], [wind], [gain], [translation], [rotation] (noise ground, a closed slab) |
| `hull` | name, inputs |
| `blob` | name, inputs, [op union / subtract / intersect], [blend], [resolution] (smooth-blended solids) |
| `split` | name, input, plane {normal, point}, [keep both / above / below], [gap] (→ `name_above`, `name_below`) |
| `scatter` | name, input (part, list = one picked per copy, `[a, b]` = a set moved as one, `"inst.*"`), on, count or density, [seed], [scale [min, max]], [rotate random_y], [align 0], [min_spacing], [max_slope], [sink], [region], [faces up], [weights] |
| `round` | name, input, mode (fillet / chamfer / smooth), [radius], [segments] |
| `deform` | name, input, mode (displace / bend / twist / taper / material), [axis y], [degrees], [toward], [scale_end], [power], [amount], [scale], [octaves], [seed], [noise], [detail], [pbr], [params] |
| `sdf` | name, shapes [{shape sphere / ellipsoid / box / capsule / cylinder / torus / chain / part / group, [op], [blend], ...}], [blend], [mirror x / z / xz], [warps], [resolution] (analytic distance-field modeling) |
| `skin` | name, chains [[[x, y, z, r] or joint, ...], ...], [joints {name: [x, y, z, r]}], [smooth true], [blend], [mirror], [warps], [resolution] (a skeleton with radii as one smooth skin) |
| `smooth` | name, input, mode (relax / remesh / subdiv), [iterations], [strength], [edge], [levels 2], [crease] |
| `decimate` | name, input, ratio or tris, [bake], [resolution 1024], [cage], [crease 60] (fewer triangles; bake the dense surface into normal + height maps) |
| `shell` | name, input, thickness, [open: face or [faces]] (hollow a solid) |
| `group` | name, inputs |
| `transform` | name, input, [translation], [rotation], [scale], [relative], [pivot] |
| `move` | [name], parts (names / `*` globs; `["*"]` = all), [translation], [rotation] or [axis + degrees], [pivot: point or anchor] |
| `place` | [name], parts, on, [at], [gap], [toward down], [each] (drop onto `on`, or mount against a wall) |
| `boolean` | name, op (union / difference / intersection), a, b |
| `radial_array` | name, input, count, [center], [degrees 360], [start_degrees], [axis y], [include_source true] |
| `linear_array` | name, input, count, offset, [local] |
| `path_array` | name, input, path, count or spacing, [smooth], [closed], [align true], [roll_step] |
| `mirror` | name, input, axis, [offset] |
| `material` | name, [input = name], [color / base_color], [roughness], [metalness] |
| `prefab` | name, prefab (catalog entry), [translation], [rotation], [scale] |
| `hide_part` / `show_part` | part (`*` matches anything: `"cut*"` hides every cutter, `"*_raw"` every raw blank) |
| `rename` | part, to |
| `delete` | name (an op output, or a baked part; its own in-place hide/material/transform edits go with it) |
| `tri_mesh` | name, verts, indices, [normals], [uvs] (baked raw triangles) |
| `collider` | name, parts (globs), shape (box / hull / mesh / none), [body static / dynamic / kinematic], [sensor] |
| `marker` | name, kind, [translation], [yaw], [size], [data], [attach] |
| `light` | name, kind (point / spot / directional), [color], [intensity], [range], [translation], [direction], [inner_degrees], [outer_degrees], [shadows], [attach] |
| `skeleton` | name, bones {name: {head, [tail], [parent], [radius]}} (points or anchors), [mirror x] or [from: lsystem / skin op, bone_length, max_depth, max_bones] |
| `bind` | name, skeleton, parts, [bone] (rigid) or [mode smooth / auto], [blend], [paint] |
| `clip` | name, skeleton, duration, tracks {bone: {rotate / translate / scale: keys or wave}}, [mirror {bone: {from, phase}}], [loop true], [interp smooth] |
| `sway` | name, skeleton, [kind wind / spring], [over clip], [bones], [duration 4], [fps 30], wind: [direction], [strength], [gust], [flutter], [frequency], [seed]; spring: [stiffness], [damping], [gravity] |
| `uvs` | part, projection (box / planar / cylindrical / spherical / metric = box in meters) |
| `repeat` | var, count or values, items (a loop, expanded before anything else) |

A `material` item's `name` may be a pattern (`"wall_*"`): it colours every
matching part in place (parts already in the model plus every name the
batch makes); a pattern that matches nothing is an error.
A `material` item may also carry `pbr` (a procedural material by name, see
**Procedural materials** below), with `params` overrides and `scale`.
A visible part with only a flat colour (no `pbr`) is a warning in
`scene_summary` and every `apply_ops` summary, and `verify materials:
"pbr"` fails it. When a plain colour is what the user wants, add
`"flat": true` to that material item and the part is left alone.

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
  - `rotation` (every item that has one): a quat `[x, y, z, w]`,
    `{"axis": [1, 0, 0], "degrees": -20}`, or Euler degrees
    `{"x": -20, "y": 90}` (applied x, then y, then z, about world axes).
    Stored as a quat, so saved files are unchanged.
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
  (`"bracket*"` takes the array copies too). `each: true` places every
  matched part on its own instead of as one body: each keeps its footprint
  and travels until it meets `on`, so 43 keys land on a sloped deck in one
  item (`{"type": "place", "name": "keys_on", "parts": ["key*"], "on":
  "deck", "each": true}`; not with `at`).
- **Frames** build in another part's own coordinates: a primitive with
  `frame: "paper"` (or `"paper:top-front"`, an anchor on that part's local
  box, same words as pivots) takes `translation` / `rotation` relative to
  that part's transform, origin at the anchor (default its own origin). It
  is a real dependency: tilt the paper (a param, a re-issue) and the framed
  parts follow. A post-graph `move` of the frame part does not carry them:
  list them in the move too. Pair with `linear_array` `local: true`, which
  steps `offset` along the source part's rotated axes (typed lines stepping
  down a tilted page).
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
  `local: true` rotates `offset` by the source's rotation.
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
- **Profiles** (extrude/revolve/sweep/loft): ring entries are points or arcs,
  `{"arc": {"center": [a, b], "radius": r, "from": deg, "to": deg}}`
  (counter-clockwise from +a; 0 -> 360 is a full circle), so curves need no
  hand-made point lists. `round: r` fillets every profile corner.
- **`sweep`** runs a closed profile along a 3D polyline (pipes, rails,
  handles, frames, hoses); bends are mitred to keep the thickness, and
  `closed: true` loops the path (a ring) instead of capping it. Profile u is
  along the frame normal (world up to start, or +z on a vertical path).
- **Smooth paths**: `sweep`, `path_array` and a path `loft` take
  `smooth: true` (or `curve: "catmull_rom"`): `path` is then a few control
  points of a centripetal Catmull-Rom curve that passes through every one,
  sampled about every 5 degrees of bend (`samples` per span overrides), so
  a hose is 4 points, not 40. `sweep` also takes `twist` (degrees from
  start to end) and `scale_end` (profile scale at the end, by length: a
  tapering horn or tail).
- **`loft`** skins a closed solid (ready for booleans) through two or more
  `sections`, which may change shape and size: a circle into a square, a
  bottle body narrowing to its neck, a hull, a horn, a blade, a tapered
  leg. Each section is a `profile` (one ring, points or arcs) at `y: h`
  (or `at: [x, y, z]`), optionally `rotation` (unrotated sections lie
  flat; `[a, b]` is world x / z like an extrude footprint) and `scale` (a
  number or `[a, b]`). Rings are resampled to the same point count
  (`samples`, default the most any section has, at least 32; corners of
  rings with fewer points stay sharp) with start points matched, so the
  skin doesn't twist. `smooth: true` runs a Catmull-Rom through the
  sections (`steps` rings per span, default 12) instead of straight
  ruled spans. With `path`, sections sit square to it at `t` (0..1 along
  it, default evenly spread; `[a, b]` along the path normal / binormal
  like sweep). `scale: 0` on the first or last section closes it to a
  point (a horn tip, a spire). A bottle:
  `{"type": "loft", "name": "bottle", "smooth": true, "sections": [{"y": 0,
  "profile": [{"arc": {"center": [0, 0], "radius": 0.035, "from": 0, "to":
  360}}]}, {"y": 0.16, ...r 0.037}, {"y": 0.22, ...r 0.014}, {"y": 0.28,
  ...r 0.013}]}`; hollow it with a smaller loft and a `difference`.
- **`path_array`** spaces copies of a part along a path (fence posts, rail
  ties, lamps along a road, chain links): `{"type": "path_array", "name":
  "posts", "input": "post", "path": [[0, 0, 0], [3, 0, 1.5], [6, 0, 0]],
  "smooth": true, "spacing": 1.2}` makes `posts_0..`, copy 0 at the path
  start and (open paths) one at the end, `spacing` rounded to fit;
  `count` instead fixes the number. Model the source at the origin with
  +x along the path and +y up: with `align` (default true) each copy turns
  so +x follows the tangent (y stays up on a level path), and the source's
  own translation is an offset in that frame (a post at y 0.5 stands on
  the path). `roll_step: 90` rolls each copy 90 degrees more about the
  tangent than the last (interlocking chain links); `closed` loops the
  path with no copy doubled at the seam. Hide the source.
- **`lsystem`** grows branching structures: trees, bushes, ferns, weeds,
  roots, coral, antlers, cracks. A `preset` gives a ready plant at real size
  (`tree` ~4.5 m broadleaf, `pine` conifer, `bush` ~1 m shrub, `fern` five
  arching fronds, `weed` tuft, `roots` growing down); every field overrides
  it, and `seed` varies the individual (same seed = same plant: a forest is
  a `repeat` over seeds). Or write your own: `axiom` + `rules`
  (`{"X": "F[+X][-X]FX", "F": "FF"}`; a list of choices is stochastic,
  `[{"p": 2, "to": ...}, ...]` weighted) for `iterations` (<= 12) rewrites,
  then a 3D turtle draws it: `F`/`G` draw a segment, `f` moves, `+`/`-`
  turn, `&`/`^` pitch, `\`/`/` roll, `|` turn back, `[`/`]` a side branch
  (thinner by `radius_scale`, shorter by `length_scale`, rolled by
  `branch_roll`, 137.5 = phyllotaxis), `!` thins, `L` places a leaf; other
  letters only rewrite. `jitter` (degrees) and `length_jitter` add natural
  variation, `tropism` + `tropism_strength` bend growth toward a direction
  (`[0, -1, 0]` droops, `[0, 1, 0]` reaches up), `taper` thins along a
  branch. Each unbroken branch is ONE smooth closed tube (`sides`), so no
  gaps at bends; branches start inside their parent. Leaves (`leaf_size`
  [width, length]) go to `name_leaves`: give the two parts separate
  materials (`"*_leaves"` patterns see them). Leaf look: `leaf_angle`
  (pitch out from the stem), `leaf_curl` (tips droop), `leaf_fold` (V
  along the midrib), `leaf_stalk` (petiole, meters).
  **Leaves are cards by default** (`leaf_mode: "card"`), the way games draw
  foliage: each leaf is a flat quad (2-6 triangles with curl and fold,
  `leaf_cross: true` adds a second card at 90 degrees for volume) with uvs
  0..1 over it, and the leaf's shape comes from an **alpha cutout**
  material, not from polygons. Give `name_leaves` a `card`-domain material
  whose output sets `opacity` (1 on the leaf, 0 around it); a part of cards
  without one warns, since every leaf then draws as a rectangle. The image's
  top (v 0) is the leaf tip, its bottom the base at the twig; it is
  stretched over the card's width x length. A leaf (`tile` 0.1 is the
  image's size in meters, the coordinate space of the shapes):
  `material_ops("leaf_card", domain: "card", tile: 0.1, resolution: 256,
  ops: [{"type": "circle", "name": "l", "center": [0.0332, 0.05], "radius":
  0.0518}, {"type": "circle", "name": "r", "center": [0.0668, 0.05],
  "radius": 0.0518}, {"type": "sdf_op", "name": "lens", "a": "l", "b": "r",
  "op": "intersect"}, {"type": "mask", "name": "shape", "sdf": "lens"},
  {"type": "segment", "name": "rib", "a": [0.05, 0.004], "b": [0.05,
  0.098], "width": 0.0015}, {"type": "mask", "name": "vein", "sdf": "rib"},
  {"type": "noise", "name": "n", "size": 0.02, "octaves": 3}, {"type":
  "ramp", "name": "green", "input": "n", "stops": [[0, "#2f5a1c"], [1,
  "#5f8f2e"]]}, {"type": "blend", "name": "col", "a": "green", "b":
  "#a8c070", "opacity": "vein"}, {"type": "output", "name": "o",
  "base_color": "col", "roughness": 0.6, "opacity": "shape",
  "transmission": 0.3}])`, then `{"type": "material", "name":
  "oak_leaves", "pbr": "leaf_card"}`. For dense crowns, draw **clusters**:
  paint several leaves (or a twig of them) into one card image and raise
  `leaf_size`, so one card stands for many leaves. A card material is one
  shared texture (like a tile), so a scattered forest reuses it.
  `leaf_mode: "solid"` models each leaf as a closed blade in its
  `leaf_shape` outline (ellipse, lance, round, heart, diamond, needle,
  maple; ~60 triangles a leaf, no texture needed): for close-up hero
  plants, prints, or when the user wants geometry. Leaf cards are open
  sheets on purpose: `validate`, `thickness` and `self_intersections` skip
  them. Models saved before cards existed keep their solid leaves.
  **Parametric** rules carry numbers on modules, so each branch knows its
  own length, width or age: axiom `"A(1, 0.08)"`, rules
  `{"A(l, w) : l >= 0.2": "!(w)F(l)[&(40)A(l*k, w*0.6)]/(137.5)A(l*0.8, w*0.8)",
  "A(l, w) : l < 0.2": "L(1)"}` with `constants: {"k": 0.6}` (values may
  be `"$param"`). Expressions: `+ - * / % ^`, comparisons, `&& || !`,
  `min max abs sqrt sin cos floor`. The turtle reads a module's first
  number: `F(len)` / `f(len)` meters, `+(deg)` and every turn, `!(radius)`
  sets the radius, `L(scale)` scales the leaf. A symbol's rules with a
  condition are tried first (in key order), then the one without, so a bare
  `A(l)` works as the fallback; first match wins; a rule only matches modules with the same number of
  parameters. Set `radius_scale` / `length_scale` to 1 when the rules size
  branches themselves. Runaway growth
  (over 400k symbols or 40k segments) is refused with a hint. Params work
  in every number (`"length": "$h"`).
- **`text`**: 3D letters (signs, labels, plaques, console legends) as one
  closed solid, standing up and readable from the front (+z), the first
  line's baseline on y = 0, `depth` thick toward -z; `\n` starts a line.
  Bundled fonts: `sans`, `bold`, `mono` (Fira, OFL); any .ttf/.otf path
  works. Engrave by subtracting it from a plate; lay it flat with a
  rotation. A missing glyph is an error naming it.
- **`terrain`** makes the ground of a level: a closed slab (heightfield
  top with smooth normals, vertical skirt walls, flat bottom at `base`)
  centred on the origin, so booleans, `place`, `grounded` and `probe_ray`
  all work on it, and metric uvs map tile materials at real size. The top
  runs from about y = 0 (valleys) to `height`; `scale` is the feature size
  (hill spacing, dune wavelength) and `resolution` the cell size (at most
  1M cells; 60 x 60 at 0.5 m builds in milliseconds). Styles: `hills`,
  `ridges` (sharp crests), `dunes` (long windward slopes, steep slip faces
  across `wind` degrees), `mesa` (`terraces` flat treads and cliffs),
  `flat` (a level slab for pads and paths only). Same `seed` = same
  ground, and a bigger `size` extends it without moving what was there.
  `flatten: [{"at": [x, z], "radius": 6 | "size": [w, d], "height"?: y,
  "falloff": 3}]` levels a pad for a building (height defaults to the
  ground's own at `at`); `paths: [{"points": [[x, z], ...], "width": 3,
  "depth": 0.3, "falloff"?}]` carves a bed level across its width that
  follows the ground lengthwise (a road at 0.1-0.3, a river at 1-2; pads
  win where they cross). `edge_falloff` eases the border down to y = 0.
  Then `place` buildings onto it (`{"type": "place", "parts": ["hut"],
  "on": "ground", "at": [x, z]}`) and colour it with a tile material that
  mixes grass and rock by `facing` up.
- **`deform`** reshapes a part's mesh for organic forms. The input is baked
  to model space and refined to edges no longer than `detail` first (default
  `scale / 8` for displace, 1/32 of the axis length otherwise, capped near
  250k triangles), so a low-poly sphere becomes a lumpy rock, not a spiky one
  and a 12-triangle box can bend. A closed solid stays a closed,
  outward-wound solid (booleans still work), UVs ride through, and open
  sheets are subdivided and deformed too. Hide the input.
  - `displace`: moves every vertex along its normal by 3D noise: `amount`
    (meters, the largest offset; default 12% of the smallest size; negative
    dents), `scale` (feature size, default 45% of the largest size),
    `octaves` (4), `seed`, `noise` `fbm` (lumps) / `ridged` (crags, cliffs,
    bark) / `turbulence`. A rock: a sphere scaled flat, then
    `{"type": "deform", "name": "rock", "input": "rock_raw", "mode":
    "displace", "amount": 0.08, "seed": 7}`.
  - `twist`: `degrees` about `axis` (default y) from its low end to its high
    end (a twisted bar, a drill bit, a rope strand).
  - `taper`: the cross-section scales from 1 to `scale_end` (a number, or
    `[a, b]` for the two cross axes in x/y/z order) along `axis`; `power`
    bends the profile (2 = stays thick, thins late).
  - `bend`: the part's `axis` extent curls into an arc of `degrees` toward
    `toward` (`x`, `-z`, ... or right/back/...; not the axis itself), the
    low end staying put. A horn is a cylinder tapered, then bent:
    `{"mode": "taper", "scale_end": 0.08}` into `{"mode": "bend", "degrees":
    120, "toward": "-z"}`; a curved blade bends a thin box.
  - `material`: real relief from a procedural material's `height` output
    (bark, scales, wrinkles, cobbles, bricks): `{"type": "deform", "name":
    "trunk", "input": "trunk_raw", "mode": "material", "pbr": "bark"}`.
    Each vertex moves along its (smoothed) normal by the height minus its
    mean, times `amount` (default 1: the channel's own meters); `scale` and
    `params` work as on a material item. Tile materials are baked once and
    sampled triplanar at real size, the same metric projection their
    colour uses, so giving the result the same `pbr` lines colour and relief
    up; object materials are evaluated per vertex (neighbourhood nodes such
    as blur see a strip there: build their height from noise / SDF nodes).
    `detail` defaults to two texels of the bake. Relief fades out within a
    few relief-depths of sharp creases (a cylinder's rim, a box edge) so
    the edge doesn't fold. Editing the material with `material_ops`
    re-derives the relief.
  Every number takes params (`"degrees": "$curl"`), and chains of deforms
  re-derive like any op.
- **`scatter`** strews copies of parts over another part's surface: rocks
  on terrain, grass tufts, mushrooms on a log, rivets on a hull, a forest.
  `{"type": "scatter", "name": "rocks", "input": ["rock_a", "rock_b"],
  "on": "ground", "count": 40, "seed": 2, "scale": [0.6, 1.4], "rotate":
  "random", "max_slope": 35}`. Points are sampled area-weighted on `on`'s
  upward-facing faces (`faces: "all"` for sides and undersides), rejected
  by `max_slope` (degrees from level), `region` (`{"min": [x, z], "max":
  [x, z]}`) and `min_spacing` (meters; default 0.8 x the widest copy's
  footprint, so copies don't overlap; 0 allows clumps), so `count` is a
  target: the result says when fewer fit. `density` (copies per m², capped
  by `max`, default 2000, at most 10000) instead of `count`. Copies are
  `name_0..`, each standing with its base (the centroid of its lowest
  slice: a trunk, not the crown) on the surface, sunk until that whole
  slice is at or below the surface's tangent plane (no downhill gap under
  an upright trunk; at most 15% of the copy's height): `verify grounded`
  passes; `sink` pushes them further in. Steep spots tilt copies into the
  slope (a big rock on a cliff fails `collisions`), so give rocks and trees
  a `max_slope`. A `"rocks_*"` material colours the copies that were
  placed (slots that didn't fit are skipped, also after a re-seed); colouring
  the source works too, copies inherit it. Separate scatters don't avoid each other (grass may grow
  inside a rock). `rotate`: `random_y` (default),
  `random` (tumbled rocks) or `none`; `align` 0 stays upright (trees), 1
  stands on the normal (mushrooms round a log with `faces: "all"`).
  `input` entries: a part; a list `["oak", "oak_leaves"]` moved as one set;
  a pattern `"tree1.*"` (every visible part it matches, a prefab instance);
  a lone lsystem name brings its leaves (`"oak"` = `["oak", "oak_leaves"]`).
  One entry is picked per copy (`weights` for odds). Set members are named
  `name_i` (the first), `name_i_leaves` (a member named after the first),
  `name_i.trunk` (prefab members). Copies keep their source's material and
  are always visible: **hide the sources**. Same seed = same layout;
  `on` is a real dependency, so lower the ground with a param and every
  copy follows (it reads `on` before post-graph moves/places). Exports share
  one mesh between copies (instancing keeps each node's rotation and scale);
  use tile pbr materials on scattered parts (object ones bake per copy).
- **`hull`**: the convex hull of several parts. Two spheres make a pill,
  eight small spheres at a box's corners make a rounded box.
- **`blob`** melts closed solids together with a smooth fillet instead of
  a boolean's sharp seam: creatures, mushrooms, melted or sculpted forms.
  Each input becomes a signed distance field (sampled from its mesh, any
  closed solid works), they combine with a polynomial smooth-min of radius
  `blend` meters (default: a fifth of the smallest input's thinnest size; 0
  = a sharp union), and the result is meshed (Manifold's level set) into
  one closed solid with the first input's material. `op: "subtract"`
  carves the other inputs out of the first with a rounded rim (a dimple, an
  eye socket), `"intersect"` keeps the rounded common volume.
  `resolution` is the voxel size (default 1/120 of the result's size);
  over ~27M samples is refused with the size that fits. Hide the inputs.
  A blob of one input with a blend just remeshes it: use `round` for that.
  `{"type": "blob", "name": "shroom", "inputs": ["stem", "cap"], "blend": 0.03}`.
- **`sdf`** models organic solids as signed distance fields: analytic
  shapes combined by smooth booleans, warped as a field, meshed once
  (Manifold's level set) into one closed solid. Cleaner and faster than
  `blob` (no meshes to sample) and the way to build creatures, mushrooms,
  bones, teeth, sculpted forms. `shapes` run in order, each joining the
  field before it by its `op` (`union` default, `subtract`, `intersect`)
  with smooth radius `blend` (per shape, else the item's, default a fifth of
  the thinnest shape; 0 = sharp):
  - `sphere` {radius, at}; `ellipsoid` {size: radii [x, y, z], at,
    rotation}; `box` {size: full [x, y, z], round, at, rotation};
    `capsule` {from, to, radius: r or [r_from, r_to]} (a rounded cone:
    limbs, horns, fingers); `cylinder` {radius, height, round, at,
    rotation}; `torus` {size: [ring, tube], at, rotation};
  - `chain` {points: [[x, y, z, r], ...], smooth} (a tube whose radius
    follows the points, a Catmull-Rom through them by default);
  - `part` {part}: a closed solid of the model sampled as a field (this is
    what `blob` does, mixable with analytic shapes);
  - `group` {shapes}: combined on their own first, then joined as one (to
    subtract a blended cluster).
  `mirror: "x"` folds the field so you model only the +x side of a
  symmetric creature (shapes crossing x = 0 should be symmetric
  themselves; `"z"`, `"xz"` for four-fold). `warps` act on the whole field
  in order, with the deform fields: `twist` / `bend` / `taper` warp space
  over the field's box at that point of the list, `displace` adds noise to
  the surface (amount, scale, octaves, seed, noise). `resolution` (voxel
  meters) defaults to 1/120 of the size, finer for thin shapes, at most
  about 12M samples; an explicit one may go to 27M. Evaluation is
  bucketed, so hundreds of shapes stay fast. A critter:
  `{"type": "sdf", "name": "critter", "mirror": "x", "blend": 0.03,
  "shapes": [{"shape": "ellipsoid", "size": [0.12, 0.1, 0.16], "at": [0,
  0.16, 0]}, {"shape": "sphere", "radius": 0.075, "at": [0, 0.27, 0.15]},
  {"shape": "capsule", "from": [0.07, 0.12, 0.09], "to": [0.09, 0, 0.11],
  "radius": [0.03, 0.022]}, {"shape": "sphere", "radius": 0.02, "at":
  [0.035, 0.29, 0.215], "op": "subtract", "blend": 0.008}]}` (body, head,
  legs, eye sockets on both sides).
- **`skin`** wraps a skeleton in one smooth surface (like a skin modifier
  or Z-spheres): tentacles, roots, worms, hands, simple characters.
  `chains` are lists of points `[x, y, z, radius]` or names of `joints`
  (`{"hip": [0, 0.5, 0, 0.08]}`); each chain is a smooth tube through its
  points with the radius following them (`smooth: false` for straight
  rounded cones), and chains melt together where they meet by `blend`
  (default half the median radius). Same `mirror`, `warps` and
  `resolution` as `sdf`. An octopus: `{"type": "skin", "name": "octo",
  "mirror": "xz", "joints": {"body": [0, 0.25, 0, 0.12]}, "chains":
  [["body", [0.14, 0.1, 0.05, 0.045], [0.3, 0.03, 0.12, 0.028], [0.42,
  0.06, 0.26, 0.012]], ["body", [0, 0.45, -0.03, 0.11]]]}` (with `xz` one
  arm makes four).
- **`smooth`** fairs or rebuilds a mesh (model space; hide the input):
  - `relax`: Taubin smoothing, `iterations` (10) rounds of `strength`
    (0.5): evens out lumps, pinches and noise without the shrink of plain
    Laplacian smoothing. Open borders stay put. Good after `deform`.
  - `remesh`: a closed solid rebuilt with even triangles `edge` meters
    long (default the input's mean edge), then relaxed along the surface.
    Edges sharper than `edge` round off. Use it before `deform` or after
    booleans that left slivers.
  - `subdiv`: Catmull-Clark of a coarse cage, `levels` 1..5 (2): a box
    becomes a smooth pebble, a few boxes a soft character. Coplanar
    triangle pairs are merged back into quads first, and edges whose faces
    meet at more than `crease` degrees stay sharp (`crease: 60` on a
    cylinder cage keeps the rims, smooths the sides); open borders stay
    sharp too.
- **`decimate`** makes a lighter copy of a dense part (an `sdf` / `skin` /
  `deform` / `blob` result easily runs to 300k+ triangles): quadric edge
  collapse down to `ratio` of the triangles or a `tris` count, keeping
  closed solids closed and the shape's silhouette. It is an op like any
  other, so it is **non-destructive**: the dense input stays in the graph
  (hide it), any change to it re-derives the decimate, and re-issuing with
  another `ratio` re-tunes it. `bake: true` keeps the dense surface's look
  on the light mesh: the result's material bake (whatever it wears: a tile
  or object pbr material, or a flat colour) gets the dense surface's
  normals blended into its normal map and its offset added to `height`, on
  a per-part atlas of `resolution` texels. `cage` (meters) is how far from
  the light surface the bake looks for the dense one (default 1.5 x the
  largest gap measured, so it rarely needs setting). A 349k-triangle
  sculpt at `tris: 1500` with `bake` reads almost like the original.
  `{"type": "decimate", "name": "statue", "input": "statue_hi", "tris":
  4000, "bake": true}` plus `{"type": "hide_part", "part": "statue_hi"}`.
  Moves, transforms, arrays, scatters and mirrors carry the bake along; an
  op that reshapes the mesh (deform, boolean, round, ...) drops it, so
  decimate last. Exports write the bake as the part's textures (per-part
  KTX2); export `lods` then simplify the light mesh further and reuse them.
- **`split`** cuts a closed solid by a plane into `name_above` (the side
  `plane.normal` points to) and `name_below`, both closed with flat caps:
  broken pots, cutaway views, parts that open or animate apart.
  `{"type": "split", "name": "vase", "input": "vase_raw", "plane":
  {"normal": [0, 1, 0], "point": [0, 0.15, 0]}, "gap": 0.02}`. `keep:
  "above"` / `"below"` makes only that half; `gap` pushes the halves apart
  along the normal (half each way). Hide the input. A plane that misses the
  part errors with the part's extent along the normal.
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
- **Loops (`repeat`)**: `{"type": "repeat", "var": "r", "count": 4, "items":
  [...]}` (or `"values": [0.0, 0.0025, ...]`, numbers or strings) copies its
  items once per value before style and params are read. In the copies,
  `{r}` inside any string becomes the value (`"name": "key{r}"`), `$r` in an
  expression or as a whole value is the number (`"=$h + $r * 0.0025"`), and
  `{r_i}` / `$r_i` is the index. `count` / `values` may read params
  (`"=$rows - 1"`) and repeats nest (`"count": "=4 - $row"` inside a row
  loop). Each copy is an ordinary item with its own template, so a later
  param change re-derives it. Two copies making the same part (same type and
  name) are an error naming the variable; an error inside a copy says which
  value it came from (`[from repeat r=2]`).
- **Prefabs as parts**: `{"type": "prefab", "name": "lamp1", "prefab": "lamp"}`
  instances a saved catalog model; its visible parts become `lamp1.pole`,
  `lamp1.shade`, ... and feed later ops like any part. The part list is
  captured when the item is applied; re-issue it after re-saving the prefab.
- **Batches are atomic**: if any item fails, the whole call rolls back
  (the error says so) and the model is exactly as before. Items may read
  parts made later in the same batch.
- **`expect`** runs checks after the batch in the same call (fields as for
  `verify`); any failure makes the call an error, but the batch stays applied.
- **Structured results stay small**: `apply_ops`' structuredContent is the
  result text plus counts and bounds; `summary: "full"` (or
  `scene_summary`) gives every part and op.
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

- `verify(parts?, includes?, grounded?, supported?, rests?, collisions?, bounds?, validate?, rebuild?, stable?, materials?, self_intersections?, thickness?, scope?)` (all
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
    part). An allow entry is `[a, b]` or `{"pair": [a, b], "depth": 0.003,
    "max": 0.5}`: excused only while no deeper than 3 mm / 50%. The PASS
    lists what each allow excused and its deepest pair, and an allow with a
    bare `*` side (`["bars_*", "*"]`) that hides an overlap over `max` gets a
    `note:` to name the real partner: a catch-all once hid typebars sunk
    into a typewriter body.

  Each check prints PASS/FAIL; structured clients get `{ok, checks}`.
- `probe_ray(origin, dir, all_hits?)`: the first part hit (or `all_hits`:
  every part, nearest first). Proves openings are open and fills are solid.
  Runs on the model itself, so it works headless too.
- `agent_views(views, width?, height?, focus?, grid?, lighting?)`: offscreen renders,
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
    reflections. The window camera adds screen-space reflections (deferred
    rendering, MSAA off, TAA averaging SSR's per-frame noise) so glossy
    parts also mirror the parts around them. Agent views have no SSR: a
    single-frame shot kept its noise as speckle on glossy/metal parts and
    its smears read as geometry. They are deferred too, single-frame with
    SMAA, so shots are deterministic (an unchanged view `diff`s to 0%).
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
  - `style: "curvature"`: each part painted by mean curvature (white flat,
    red convex, blue concave, saturated where the radius of curvature is a
    sixteenth of the part or tighter): lumps, pinches, dents and creases on
    organic surfaces stand out where shading hides them.
  - `dims: true`: the framed box's width and height and a scale bar, at true
    scale in orthographic views. Perspective views have no single scale:
    they state the model's box size ("not the view") and a scale bar valid
    only at the look-at point's depth.
  - `diff: true`: compares with the previous render of the same view, size,
    focus and style: magenta where something now is, cyan where something
    was, the rest greyed, plus the changed share and pixel box.
- `inspect(measure?, section?, mass?, thickness?)`: numeric senses, any
  combination per call.
  - `thickness: {parts?}`: per visible part, the thinnest point straight
    through it along the inward normal (and where), the 5% and median
    thickness, and whether its surface passes through itself: a limb
    pinched to nothing, a wall too thin to print.
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
  the cursor by default. Editing after a jump branches. With several
  agents on one server, each node records who made it (`history` shows
  `by <client>`), and `undo` refuses to revert another agent's step
  (`undo(force: true)` when that is intended).
- `diff(from, to?)`: ops added (+), removed (-) and changed (~, with the old
  parameters) between two nodes (default `to`: the live model), plus param
  changes.
- `cherry_pick(from, ops?)`: replay ops from another node onto the live model
  (all new or changed ops and params, or just the named ops/params). A
  picked op brings the param values it was built with, so the graph stays
  consistent. After a jump, this replaces re-issuing the old branch by hand.

**Model, catalog, output:** `new_model(name)` (the old model stays one undo
away), `set_model_name(name)` (the prefab entry name), `save_prefab(description,
tags, prompt, force?)` (reports when it overwrote an entry; refuses, unless
`force`, when the catalog file changed since this session loaded or saved
it: another agent or server saved it in between),
`load_prefab(name)` (answers with a brief: counts, visible parts, params,
named moves, warnings), `catalog_list` and `catalog_describe` (`delete: true` removes an entry;
its file moves to `prefabs/removed/`; each entry also
lists its knobs: params with values and named moves/places, read from the
file),
`export_model(path, "gltf" (default) | "glb")` (skips hidden parts).

**Game export (`export_model(..., optimize)`):** `optimize: "game"` (or an
object of flags over it; `"none"` / omitted = plain) prepares the model for
Bevy and returns a before -> after report (tris, verts, meshes, materials,
batches, bytes):

- `dedup_materials` (on): identical looks share one glTF material, so Bevy
  shares the `StandardMaterial` handle and can batch.
- `cull_contacts` (on): drops triangles whose samples, nudged outward, land
  inside another visible solid (bottoms on the deck, insets in frames).
- `visible_from: [{min, max}]` (opt-in): keeps only triangles a ray can see
  from some point in those camera boxes (`view_samples`, default 256), e.g.
  an interior drops its hull's outer skin. Blended parts don't block.
- `instance` (on): translated copies share one mesh with their own node.
- `merge: "by_material"` (opt-in): one mesh per material; `merge_keep:
  ["door*"]` keeps parts the game needs by name.
- `weld`, `small_indices` (u16), `orm` (AO packed into metal-roughness R),
  `emissive_strength` (KHR_materials_emissive_strength) are on in `game`.
- `lods: [0.5, 0.2]` (opt-in): every part with at least `lod_min_tris`
  (300) triangles gets simplified copies `<part>.lod1`, `.lod2` (quadric
  simplification that keeps seams and uvs, so they reuse the full part's
  textures) as sibling nodes. Each node's `extras.lod = {level, near,
  far}` gives the camera distances (m) it is meant for: `lod_distances:
  [20, 60]`, or by default 15x and 45x the part's size. Instanced copies
  share the LOD meshes. In Bevy, put a `VisibilityRange` from those
  numbers on each node (`start_margin: near..near`, `end_margin:
  far..far`, or with a few meters of crossfade).
- Nodes carry `extras {part, material, comments, lod?}` for game-side lookup.

**Lights:** `{"type": "light", "name": "bulb", "kind": "point",
"translation": [0, 1.4, 0], "color": "#ffd8a8", "intensity": 800,
"attach": "lamp_shade"}`. Units are Bevy's: lumens for point/spot, lux for
directional (defaults 2000 / 3000); at Bevy's default camera exposure
(EV100 9.7, daylight) indoor lights need tens of thousands of lumens to
read, so a game lighting an interior lowers its exposure instead. `direction`
aims spot/directional lights (default down); spot cones take
`outer_degrees` (35) and `inner_degrees` (0.8 x outer). `attach` makes the
light ride its part through moves and places; prefab instances bring their
lights (`inst.bulb`). Colours take style tokens (`"@glow"`). Lights export
as KHR_lights_punctual (Bevy's loader spawns them; `shadows` goes in node
extras since glTF has no flag). The preview lights them on top of the
studio; `agent_views(..., lighting: "scene")` renders with the model's
lights only (studio off, faint environment) to judge how the level is lit.

**Game data (colliders, markers):**

- `collider` rules give parts collision shapes:
  `{"type": "collider", "name": "level", "parts": ["*"], "shape": "box"}`
  then narrower rules (later wins per part): `mesh` for static level
  geometry that needs its real shape (ramps, arches), `hull` for props,
  `body: "dynamic"` for things that fall and get pushed (hull or box only),
  `sensor: true` for triggers, `none` to exempt (decals, foliage).
- `marker` items are named points for game code: spawns, waypoints, item
  spots, cameras, trigger zones (`size` = a box volume). `yaw` faces them
  (degrees about +y, 0 = +z), `data` carries any JSON (`{"team": 1}`),
  `attach` makes them ride a part's moves.
- Both come along with prefab instances (`b1.grab`; the prefab's rules
  apply first, so the scene can override: `{"parts": ["b2.*"], "shape":
  "box"}`), follow moves, and draw as gizmos in the window and agent views
  (markers magenta, colliders green static / orange moving / blue sensor).
- Export (any `optimize`): a `box`/`hull` collider is a node
  `<part>.collider` at the part's centre with `extras.collider`
  `{shape: "cuboid", half_extents} | {shape: "convex_hull", points}` plus
  `body` and `sensor`; a moving body's render node is its child so they
  move together. `mesh` sets `extras.collider = {shape: "trimesh"}` on the
  part's own node. Markers are empty nodes with `extras {marker: kind,
  size?, data?}`. The report counts both. With `merge`, keep moving bodies
  and mesh colliders out via `merge_keep` (the report warns).
- In Bevy, enable the glTF extras (`GltfExtras` comes with the scene) and
  map them, e.g. with Avian:

  ```rust
  fn colliders(q: Query<(Entity, &GltfExtras), Added<GltfExtras>>, mut c: Commands) {
      for (e, x) in &q {
          let Ok(v) = serde_json::from_str::<serde_json::Value>(&x.value) else { continue };
          let Some(col) = v.get("collider") else { continue };
          let shape = match col["shape"].as_str() {
              Some("cuboid") => { let h = &col["half_extents"];
                  Collider::cuboid(2.0 * h[0].as_f64().unwrap_or(0.5) as f32,
                                   2.0 * h[1].as_f64().unwrap_or(0.5) as f32,
                                   2.0 * h[2].as_f64().unwrap_or(0.5) as f32) }
              Some("convex_hull") => Collider::convex_hull(
                  serde_json::from_value::<Vec<Vec3>>(col["points"].clone()).unwrap_or_default())
                  .unwrap_or_else(|| Collider::sphere(0.1)),
              _ => { c.entity(e).insert(ColliderConstructorHierarchy::new(
                  ColliderConstructor::TrimeshFromMesh)); continue }
          };
          let body = match col["body"].as_str() {
              Some("dynamic") => RigidBody::Dynamic,
              Some("kinematic") => RigidBody::Kinematic,
              _ => RigidBody::Static,
          };
          c.entity(e).insert((shape, body));
          if col["sensor"].as_bool() == Some(true) { c.entity(e).insert(Sensor); }
      }
  }
  ```

  Markers the same way: read `extras.marker` and spawn your own
  components at the node's `Transform`.

**Skeletons and animation:** Rig a model after its shape and materials are done: shape, then materials,
then the rig. Rig items are `apply_ops` items like any other (upserts,
params, atomic, undoable). They run after the graph, as `move` does, so the
geometry recipe stays as it is. One pose evaluator serves every consumer:
agent views, verify, inspect, export and the window.

- **Bone frames are world-aligned at rest.** A bone has a `head` (its pivot),
  a `tail` and a parent. Every rest rotation is the identity, so a clip
  rotation `{"x": 25}` means "pitch 25 degrees about world x from rest", with
  no bone roll to think about. Clip tracks are offsets from rest, composed
  down the tree.
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
- **Auto-rigs (`from`)**: instead of listing bones, `from` names an op.
  - **An `lsystem`**: bones run along its branches (`trunk_0..`,
    `branch7_0..`), one every `bone_length` meters (0.5), down to `max_depth`
    branch levels (3), within `max_bones` (128, at most 256), thickest
    branches first. Finer twigs ride the bone they grow from.
  - **A `skin` op**: one bone per chain point. Named joints keep their names
    and other points are `c{chain}_{point}`. The radii come from the chain,
    and a skin with `mirror: "x"` gets `_l` / `_r` bones.
  - Extra `bones` may hang off the generated ones.
- **`bind`** attaches parts to a skeleton:
  - `bone: "forearm_l"` binds rigidly: a door on its hinge, a robot arm.
  - `mode: "smooth"` gives per-vertex weights from the bone envelopes,
    diffused over the mesh: two legs side by side don't bleed into each
    other. `blend` is the meters weights blend across a joint, and `paint:
    [{bone, region: {min, max}}]` forces a region onto one bone.
  - `mode: "auto"` on an lsystem rig gives exact weights along the branches,
    and every leaf card rides its twig's bone rigidly. Bind both:
    `"parts": ["oak", "oak_leaves"]`. On any other rig, `auto` is `smooth`.
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
- **`sway`**: dynamic motion, baked into a clip of its own name. Every tool
  that plays clips plays it.
  - `kind: "wind"` (the default):
    `{"type": "sway", "name": "wind", "skeleton": "oak_rig", "direction":
    [1, 0, 0.3], "strength": 0.6, "gust": 0.4, "flutter": 0.5,
    "duration": 4}`.
    - Thick bones bend slowly and a little, thin ones faster and further,
      and the tips flutter.
    - `strength` is 0.5 for a breeze, 1 for a strong wind, 2 for a gale.
    - Every frequency snaps to whole cycles of `duration`, so the loop is
      seamless.
  - `kind: "spring"` is secondary motion: tails, ears, antennae, a hanging
    sign.
    - Each bone's tip lags behind where `over` (a looping clip) puts it, on
      a damped spring (`stiffness` 80, `damping` 8; `2 * sqrt(stiffness)`
      is no bounce).
    - `gravity` droops it, and a `strength` adds wind.
    - It bakes three loops of `over` and keeps the last, so it loops.
  - `bones: ["tail*"]` limits which bones sway; the rest only follow.
  - `duration` defaults to 4 s, or to the `over` clip's duration. With
    `over` it must be a whole number of that clip's cycles.
  - `fps` sets the export rate (30).
- **Seeing motion**: `agent_views` with `clip` and `times: [0, 0.25, 0.5,
  0.75]` renders a **filmstrip**: each view at each time, tiled with labels.
  This is how to judge a walk. `time` gives a single moment.
  - `style: "bones"` overlays the skeleton.
  - `style: "weights"` with `bone` shows a heat map of that bone's weights.
- **Checking motion**: `verify`.
  - `weights: true`: every vertex of a smooth part is weighted, the weights
    sum to 1, and there are at most 4 bones per vertex.
  - `animation: {clip, samples: 16, collisions, floor, loop, stretch}` runs
    at each sample time and fails naming the time and bones:
    - `loop`: the pose at 0 matches the pose at the end;
    - `floor`: nothing sinks below y = 0;
    - `collisions`: a limb through the body;
    - `stretch`: skinning tears or collapses a triangle.
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
    for the runtime plugin.
  - `instance`, `merge`, `cull_contacts` and `lods` leave rigged parts
    alone.
- **Runtime plugin (`crates/modeler_bevy`)**:
  - `app.add_plugins(modeler_bevy::ModelerPlugin)` runs the same wind and
    spring simulation live (`crates/modeler_anim`), after `AnimationPlayer`,
    so a spring tail layers over a playing walk. Each instance gets its own
    phase.
  - The `Wind` resource steers every rig: `enabled`, `direction`,
    `strength`, `gust`.
  - Play the baked sway clip *or* keep the live sway (`Sway::live`), not
    both.
  - It also turns `extras.collider` into `ModelerCollider` and
    `extras.marker` into `ModelerMarker`.
  - `cargo run -p modeler_bevy --example viewer -- <assets dir>
    <model.gltf>` looks at an export.
- **Limits**:
  - Scattered copies of a rigged tree don't get their own skeletons. Rig
    the tree in its own document, export it, and place the copies in the
    game, where the plugin sways each one with its own phase.
  - A skin's `warps` and a `mirror` other than `"x"` are not followed by
    auto-rigs.

**Scenes (open documents):** `scene(action, name?)` keeps several models
open at once, each with its own ops, history and style; every other tool
acts on the active one. `list` (active marked `*`, which prefabs each
instances), `open` (an open doc, else the catalog entry, else a new empty
doc), `switch` (open docs only), `close`. Build a map from pieces: model
`console` in its own doc, `open` `bridge_map`, and place
`{"type": "prefab", "name": "c1", "prefab": "console"}` items. A prefab
item resolves to the **open doc of that name first** (else the catalog
file), and switching back to a scene re-syncs every instance from its
source (new parts, edits, style), so pieces stay live while you iterate.
A prefab item's `style`: omitted keeps the piece's own style; `"scene"`
re-derives it with the scene's tokens layered over its own (scene wins;
re-themes with `style set` on the scene); an object patches tokens for that
instance (`{"colors": {"primary": "#c33"}}`, a red-alert variant). Only
items that read tokens (`@color`, presets) follow. The inactive docs
autosave to `open_docs.json` and come back with `--restore`; the preview window
shows tabs for open docs (click to switch).

**Annotations:** `add_annotation(part, text, face?, vertex?, position?, material?, node?)`
(omit the optionals to consume a spot the human clicked; `material` + optional `node`
comments on a procedural material or one of its nodes; humans also comment on
outliner groups, listed with their parts), `list_annotations`,
`delete_annotation(id)`.

**Feedback:** when a call surprises you (an error that didn't say what to
do, a field that didn't work as described, a workaround you needed), send
one or two sentences with `session_log(feedback: "...")`: what you tried,
what happened, what you expected. The server also logs every failed call
(error, a clip of its arguments, failures in a row) and every call over 8 s
on its own. All of it goes to `<catalog dir>/feedback.jsonl` on this machine
(git-ignored in a project's `.modeler/`); `session_log(report: true)` or
`modeler --feedback-report` folds it into failures grouped by cause (numbers
and quoted names blanked, so one problem on many parts is one line), slow
calls and notes.

**Session:** `session_log(limit?)` is the per-call record (tool, ok/error,
ms). Every mutation marks the session for autosave (ops, params, history,
annotations; op-derived parts are left out and re-derive on restore), and the
app writes `session.json` once edits pause for 0.5 s and on exit, so the write
never sits on a tool call. A server started with `--restore` (or
`MODELER_RESTORE=1`) restores it on startup; a plain `modeler` (what the
MCP config runs) starts empty and first moves the last autosave aside to
`session.prev.json` (+ `.prev.history.jsonl`, `open_docs.prev.json`, the
materials' `session.prev.json`), so its edits never overwrite it; a later
`--restore` brings the kept one back when nothing newer was written.
`mcp_driver.py` always passes `--restore`, so daemon restarts keep the work. The history tree goes to an append-only
journal beside it (`session.history.jsonl`, one node per line): a save
appends only the new nodes, and rewrites the file only after undo branches
or pruning, so the autosave stays small however long the session runs. `save_prefab` only
names and indexes it.

## Style tokens

A model carries a **style**: design tokens items read instead of literal
values, so a generated scene has one cohesive look and restyles in one call
(`src/style.rs`). Set it first, then build with tokens.

- `style(action, tokens?, name?, ...)`: `show` (tokens + a swatch image),
  `set` (merge `tokens`; a null removes a token), `replace`, `clear`, and
  the library (`<data>/styles`): `list`, `describe`, `save` (name,
  description, tags, prompt), `load` (becomes the model's style), `delete`.
- Tokens: `brief` (the feel in words, shown in `scene_summary`),
  `colors` (`"@primary"` anywhere a colour goes, with `.darken(x)`,
  `.lighten(x)`, `.mix(@b, x)`, `.alpha(x)`, `.saturate(x)`,
  `.desaturate(x)`; colours may alias `@others`), `materials` (presets, used
  as `{"type": "material", "name": "top", "style": "plastic"}`; the item's own
  fields win), `numbers` (`"$bevel"`, like params; a model param of the same
  name wins), `defaults` (per item type, fields filled in when omitted:
  `{"round": {"radius": "$bevel"}, "revolve": {"segments": "$segments"}}`).
- There are no built-in styles: each project defines its own. A worked
  example (a pastel toy look), then an item that uses it:

  ```json
  {"name": "style", "args": {"action": "set", "tokens": {
    "brief": "chunky toy: pastels, soft rounded edges, low-poly curves",
    "colors": {"primary": "#e07a5f", "secondary": "#81b29a", "wall": "#f4e1d2",
               "ink": "#3d405b", "trim": "@primary.darken(0.25)"},
    "materials": {"plastic": {"color": "@primary", "roughness": 0.45},
                  "metal": {"color": "@ink.lighten(0.3)", "roughness": 0.35, "metalness": 1}},
    "numbers": {"bevel": 0.04, "segments": 10},
    "defaults": {"round": {"radius": "$bevel"}, "revolve": {"segments": "$segments"}}}}}
  ```

  `{"type": "material", "name": "table_top", "style": "plastic"}`,
  `{"type": "material", "name": "ball", "color": "@secondary"}`,
  `{"type": "round", "name": "top", "input": "top_raw", "mode": "fillet"}`
  (radius from `$bevel`). Then `style set` with new colours or `style load`
  of another saved style restyles the whole scene with no op edits.
- Items that read the style keep their unresolved form (a template, like
  params): `style set` / `load` re-derives every one whose resolved form
  changed, as one history step (undo restores the old style). Items applied
  before a style existed have no template and don't follow later styles.

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
  (box-unwrapped islands at one texel density, dilated gutter). `card`:
  one `tile`-meter image (like a tile, coordinates in meters, top row =
  v 0) laid once over each part's own 0..1 uvs instead of remapped: leaf
  cards, decals, signs. One shared texture, like a tile.
- **Node families.** Coords: input, transform, combine, split, polar,
  kaleido, warp, tile_grid (ports local / cell / id / edge), repeat,
  triplanar. 2D SDFs: circle, rect, segment, polygon, star, hexagon, arc,
  rings. 3D SDFs: sphere, box, torus, capsule, cylinder, plane. SDF ops:
  sdf_op (union / intersect / subtract and smooth_ versions), sdf_round,
  onion, displace, mask (fill / stroke / glow, anti-aliased). Noise: noise
  (value / gradient; fbm / ridged / turbulence), worley (f1 / f2 / edge /
  id). Maths and colour: math, remap, random (per id), color, ramp, hsv,
  blend, levels, gradient (linear / radial; 3-value points read object
  position, a fade across a part), facing (cosine to a direction: dust on
  tops, moss on one side), image (a PNG, `size` meters per repeat; ports
  colour / alpha / value). Neighbourhood: blur, dir_blur (smear along an
  angle or a `flow` field: streaks, brushed metal, hair),
  normal_from_height, ao_from_height, curvature, dilate, islands (connected
  regions of a mask: `id` per island for `random`, `area`).
- **`scatter`** is a tile sampler: shapes (ellipse, circle, rect, feather,
  leaf) strewn on a jittered grid of `cell` meters, `per_cell` each, with
  random `scale`, `angle_jitter` and `density`, pointing along `angle` or a
  `flow` field, stacked `layering: "random"` or `"flow"` (upstream ones on
  top: feathers, scales, shingles). The top instance drives the ports:
  `mask`, `local` (-1..1 inside it, base to tip along y: draw any detail
  with other nodes on these coords), `id` (per instance, for `random`),
  `height` (a dome) and `edge` (meters inside). Tileable.
- **Mesh-aware masks** (object domain on a part): `mesh_ao` (occlusion
  from the part's own mesh: creases and hollows darken) and
  `mesh_curvature` (0.5 flat, ports `convex` for edge wear and `concave`
  for dirt in crevices), from a voxelized copy of the part and the visible
  parts near it (a wing darkens the flank under it); without a mesh (tile domain, the
  preview sphere) they read neutral.
- **`output`** names the channels: base_color, roughness, metallic,
  height (meters; the normal is derived from it, times normal_strength,
  unless `normal` is given), ao, emissive, opacity. Missing channels use
  defaults. Normals are in the glTF convention (+y up the image). Whole-
  surface factors (numbers): `clearcoat` / `clearcoat_roughness`
  (lacquer, wet), `anisotropy` / `anisotropy_rotation` (degrees; brushed
  metal, hair, satin), `transmission` (light through leaves, paper, skin),
  `specular_transmission` + `thickness` + `ior` (glass, water). They show
  in the preview and export as KHR_materials_clearcoat / anisotropy /
  diffuse_transmission / transmission / volume / ior.
- **Alpha.** `opacity` below 1 renders by `alpha`: `"mask"` (a cutout:
  each texel fully there or not at `alpha_cutoff`, default 0.5; leaves,
  grass, chain-link, lace; stays in the opaque pass, casts cut-out
  shadows, needs no sorting), `"blend"` (see-through: glass, smoke, ghosts)
  or `"opaque"`. Default: mask when the opacity is nearly all 0 or 1, else
  blend. `double_sided` (default on for mask) draws both faces, so a card
  shows from behind. Exports write glTF `alphaMode` MASK / BLEND,
  `alphaCutoff` and `doubleSided`; a mask texture's mipmaps keep its
  coverage, so foliage doesn't thin out and vanish at a distance.
- **Layers**: `output` takes `over: [{"output": "dirt_out", "mask":
  "dirt_m"}]`, each a whole other `output` node blended in (every channel)
  where its mask is 1, in order: moss over stone, rust over paint, mud over
  feathers. The main output is the one no other output layers in.
- **`expect`** checks: `tileable` (bakes the tile shifted by half a tile and
  compares: a non-periodic node fails, naming the channel), `ranges` (finite,
  0..1), `albedo` (non-metal luminance in 0.02..0.9 at the 1st / 99th
  percentile, or your range), `normals` (unit, facing out), `coverage`
  ([{node, min, max}]: share of the area where that node is above 0.5;
  a failure gives the node's 5/50/95% values and, for a node with an
  `input` (levels, remap), the input value that would hit the wanted
  coverage, so the threshold is one edit away).
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
  the op, so models and prefabs bake identically without the library; a
  `material_ops` edit re-embeds the new graph in every item of the active
  model that uses it (one history step), so parts follow without re-issuing. Materials follow lineage like
  colours; the uv mapping runs after the whole graph, so cut faces and copies
  are mapped too (it replaces any `uvs` projection on those parts). Factors
  become 1 (white) unless given, then multiply the textures.
- **Export:** `export_model` writes the baked textures (base colour,
  metallic-roughness, normal, occlusion / emissive when driven) and a
  TANGENT attribute. **Prefer `.gltf`** (the default when `format` is
  omitted, unless the path ends in `.glb`). Its textures are **shared KTX2
  files**: GPU block-compressed (BC7 colour, BC5 normals, BC4 occlusion)
  with a full mip chain and zstd supercompression, which Bevy's default 3D
  features (`ktx2`, `zstd_rust`) load as they are and keep compressed on the
  GPU (about a quarter of a PNG's memory, no shimmer at a distance). Each
  goes to the `textures/` folder at the root of the `assets/` folder the
  model is exported into (else `textures/` beside the .gltf), named by
  material, channel and a hash of its content
  (`stone-base_color-1a2b....ktx2`), and the .gltf points at it by uri.
  Every model using the same bake points at the same file, so Bevy's asset
  server loads it once. `.glb` (one self-contained file) packs the same KTX2
  into its buffer (`mimeType: image/ktx2`, which Bevy's loader reads): still
  GPU-compressed, but each .glb has its own copy, so prefer .gltf for a kit.
  `optimize: {"textures": ...}` overrides: `"ktx2"` (shared files), `"png"`
  (shared PNGs), `"embed"` (KTX2 in the buffer), `"embed_png"` (PNG in the
  buffer, for viewers without KTX2 such as Blender). Only **tile** materials
  (the default domain: metric, repeating) can be shared across models; an
  **object** bake is fitted to one mesh, and the export report counts
  those. Use tile materials for anything a kit reuses (stone, bark, leaves,
  metal) and keep object materials for one-off hero parts.
- **History:** `undo` / `redo` / `jump` / `history` take `doc:
  "material:<name>"` (or `"material"` for the active one).
- **Bake cache:** bakes are cached by key (material, params, and for
  object materials the part's mesh and neighbours), least recently used
  evicted past 1 GB; the preview and exports bake all parts in parallel
  first, so only parts whose key changed re-bake after an edit.
- **Parallelism:** a level's nodes run in parallel, every kernel is
  data-parallel over samples (rayon), results are deterministic at any
  thread count, and a memo cache keyed by node + input keys + domain
  re-runs only what a tweak touches (results report "N computed").

## Semantics worth knowing

- **In-place ops**: a material, transform or visibility op whose output is
  its own input is legal; upsert never replaces an op the new op reads from.
  Re-issuing an in-place transform or visibility edit replaces it (a
  relative transform does not apply twice); in-place materials stack, and
  the last wins.
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
Auto-drift stops once the human steers; a new model re-frames the camera
around true world bounds (bounds include part transforms), while edits to
the same model keep a camera the human steered.

Layout (`ui::apply_layout`; the 3D camera renders to an image the size of
what is left (`View3d`), shown by a UI node, so `screenshot` never includes a
pane and screen-space reflections line up: a camera viewport inside the
window misplaced them). Every pane starts closed, just the 3D view
(`MODELER_OUTLINE=1` / `MODELER_STYLE=1` / `MODELER_MATERIALS=1` open one at
start; the keys toggle them):

- **O**, the outliner, top of the left column (40%, `PANE_FRACTION`): files
  first (`outline::build_files`): the active document (expanded into its
  part tree), the other open documents (click to switch) and a `library`
  group of catalog prefabs not open (click to open). Inside the document
  the scene is a hierarchy (`src/outline.rs`): prefab instances and array
  runs fold into expandable groups (an instance row says `prefab <name>`
  and its `open` button opens the source), hidden parts (cutters, blanks,
  sources) sink into one dimmed group, each row shows its material. Click a
  row to select it (its parts pulse in 3D), click a selected group or its
  chevron to open it, double-click to frame it; the wheel scrolls.
- **S**, the style pane, under the outliner (`preview/style_pane.rs`): the
  model's tokens, read-only: the brief, colour chips with hex, material
  presets, numbers and defaults. It follows every `style` call.
- **G**, the grid (`preview/grid.rs`, `MODELER_GRID=1` at start): every
  open document (up to 9) side by side in the 3D area, so a human can watch
  several agents build at once. Each tile is labelled with the document,
  its visible part count, the agent that made its latest step (`by
  <client>`) and when it last changed; "(active)" marks the active one.
  Click a tile to switch to that document and leave the grid. Cheap by
  design: each tile is its own camera on its own render layer rendering
  into its own image, active only for a few frames after its document
  changes (or the tile resizes), so an idle grid renders no 3D at all; the
  window camera (TAA + SSR) pauses while the grid is up, tile cameras skip
  SSR / TAA / SSAO and shadows, and the app loop drops to ~30 fps. A
  `screenshot` in the grid captures the window (tiles and labels).
- **M**, the material strip along the bottom under both: the active
  material's graph by dependency level with a live thumbnail per node, a lit
  sphere and the channels; clicking a node shows its output large (click
  again for the material).

Comments use a Feathers popup (`preview/comments.rs`) listing the target's
existing comments (each deletable): click a part in 3D, right-click an
outliner row (a group comments on all its parts) or a material node / the
preview sphere; Enter commits, Esc cancels. The UI uses Bevy Feathers
(`preview/ui.rs`): the dark theme, Fira fonts, pane headers with a close
button, and a toolbar at the 3D view's top right whose Outliner / Style /
Material / Grid toggles match O, S, M and G.

## Dev notes / gotchas

- Agents driving the server over stdio (frozen MCP tool list) should run
  `python3 mcp_driver.py --daemon --windowed` once in the background and then
  `python3 mcp_driver.py --connect --calls <file>` per batch: one long-lived
  server means the preview window stays open and state is live between
  invocations (autosave covers even a daemon restart). `{"name": "quit"}`
  stops it. The default throwaway mode spawns and kills a server per
  invocation — window flashes open/closed, state restored from autosave.

- **Several agents can share one daemon.** Each connection has its own
  thread and calls interleave one at a time, so one agent's long batch
  doesn't block the others. Every call carries its client (the agent
  session behind the calling shell, or `--client NAME`) in the MCP
  `_meta` (`modeler/client`, `modeler/doc` for `--doc`), and the server
  (`Session::begin_call` / `end_call`):
  - keeps each client's own active document, switching back when another
    client switched away (said once, then quietly);
  - credits history nodes and op writes to the client, tells it what other
    clients changed in its document since its last call ("since your last
    call, B made 2 edit(s) ..."), warns when a batch replaces an op another
    client wrote, and refuses an `undo` of another client's step unless
    `force: true`;
  - `--doc NAME` pins a run to a document (opened, or new, if not open).
  Calls without `_meta` (a plain MCP client, the human's window) behave as
  before. Separate servers sharing one catalog are guarded by the session
  lock (autosave) and `save_prefab`'s changed-file check.
- The daemon keeps the server's stderr (panics, bake errors) in
  `<catalog dir>/daemon.log` (rotated to `daemon.log.1` past 1 MB at each
  spawn; `MODELER_STDERR=<file>` overrides). The server's last line there
  is `modeler: exit: <reason>` (window closed / stdin EOF (MCP client gone)
  / panic in thread ...), which the restart note quotes.
- **One main autosaver per catalog dir**: at startup the server writes
  `<catalog dir>/session.lock` with its pid. A second live server finds it
  held and autosaves to its own **slot** instead: `session.<slot>.json`,
  `open_docs.<slot>.json` and the materials' `session.<slot>.json`
  (`MODELER_SLOT`, which the driver sets per daemon socket, else
  `pid<pid>`). On startup it restores from its slot when that exists, so a
  restarted second daemon gets its work back. Its stderr says "autosave
  SLOT", `scene_summary` has an `autosave:` line, and the driver tells each
  client once in its first result. A dead holder's lock is taken over, also
  mid-session (from then on it writes the main files); a clean exit removes
  it.
- rmcp 1.8 server runs a tokio current-thread on a spawned thread; the Bevy
  app owns the main thread (winit on macOS). Bridge = tokio mpsc + oneshot.
- `--headless` uses MinimalPlugins — don't add ScheduleRunnerPlugin again.
  `--offscreen` is DefaultPlugins with no primary window and WinitPlugin
  disabled, driven by ScheduleRunnerPlugin at 60 Hz; it exits on stdin EOF
  like headless.
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
  `pkill -f target/debug/modeler` — the shared daemon (behind the MCP
  proxy) respawns the server from the binary on disk on the next tool call. The respawned session AUTOSAVES
  and RESTORES: mutations are written to `session.json` in the catalog dir
  0.5 s after the last edit (and on exit; a hard kill loses at most that), and a server started with `--restore` (the driver always does) restores it on startup, so
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
