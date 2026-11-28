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

This file holds the operating rules and the map. The reference lives in
`docs/ops/<type>.md` (one per `apply_ops` item type) and `docs/topics/*.md`,
and the server serves it on demand: **`describe_op(kind)`** returns the
item's guide, an example and its schema, and **`describe_op("topic:<name>")`**
a topic (`describe_op()` lists both). Read those before using an op you
haven't used this session rather than guessing fields. The MCP `initialize`
response (`instructions` in `src/bin/modeler/server/tools.rs`) carries the
critical rules; keep it, this file and the docs in sync when semantics change.

## Getting started (agent)

- **One server, one window for every session.** The MCP config runs
  `python3 mcp_driver.py --proxy`: each Claude session gets a thin stdio
  proxy, and every proxy shares one daemon (`/tmp/modeler-mcp.sock`, started
  windowed on first use, detached). Each session is its own client (its own
  active document, authorship, undo guards). A client-side timeout does not
  mean the call failed: check `history` / `scene_summary` before retrying an
  `apply_ops`.
- **Pass `doc`** (an open document's name) on every tool that reads or
  changes a document. Subagents of one session share one proxy and so one
  active document. A `doc` naming an idle-closed, parked or catalog document
  reopens it; an unknown name errors with a did-you-mean.
- **`new_model` never displaces work**: a non-empty document stays open and
  the new one starts beside it; a name that's already open is refused.
- **Projects**: each call carries its project (the game repo the session's
  working directory is in, else the shared library), and the catalog,
  exports and prefab lookups follow it for that call.
- One daemon per agent instead (agents mustn't see each other's documents):
  `MODELER_SOCKET=/tmp/modeler-<name>.sock` in that session's MCP `env`.
- **Working efficiently**: verify numerically (`verify`, `inspect`) between
  steps and render once per model at the end with one tiled `agent_views`
  (`["front", "right", "top", "iso"]`). Many identical props: one prefab and
  `prefab` instances, never hundreds of separate items.
- After `cargo build`, `pkill -f target/debug/modeler` (the daemon respawns
  the server from disk on the next call, restoring from autosave); a `quit`
  call stops everything. New MCP tools only appear in a new session.
- **Without MCP tools**, `python3 mcp_driver.py --daemon --windowed &` once,
  then `python3 mcp_driver.py --connect --calls <file>` (a JSON array of
  `{"name", "args"}`) per batch. Without `--windowed` it runs `--offscreen`
  (renders work, no window); `--headless` drops rendering.
- **In CI** (Linux, no GPU): the `cumsauce/modeler@main` action (README, "In CI").

Details (proxy internals, slots, progress notifications, the driver's flags):
`docs/topics/daemon.md`.

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
     the server's working directory with a `.modeler/` folder, a
     `Cargo.toml` beside an `assets/` folder, or a `project.godot`. Relative
     `export_model` paths land in `<project>/assets/` (`"models/crate.glb"`
     -> `assets/models/crate.glb`; a leading `assets/` is not doubled), or
     at a Godot project's root (`res://`) when it has no `assets/`. Outside a
     project a relative path lands in the shared library's `exports/`
     folder, never the daemon's cwd; the result prints the absolute path. `.modeler/.gitignore` is written once to
     keep the per-machine autosaves (`session.json`, `open_docs.json`) and the
     prefabs' undo histories (`*.history.json` sidecars) out
     of git. `catalog_list`'s first line says which library is in use.
   - **Otherwise** the shared library
     `~/Library/Application Support/friendslop/modeler/` (this repo: no
     `assets/`, so testing uses the shared one). `FRIENDSLOP_DATA_DIR`
     overrides both (tests).

## MCP tool surface (34 tools)

- Build: `apply_ops` (every geometry, material, rig and game-data change),
  `material_ops` (procedural material graphs), `style` (design tokens).
- Look up: `describe_op`, `scene_summary` (call FIRST and after anything
  confusing), `ops`, `op_get`.
- Check: `verify`, `inspect`, `probe_ray` (numbers, headless), then
  `agent_views` / `screenshot` / `material_view` (images; **always look at
  one image after building**). `topic:verify`, `topic:inspect`,
  `topic:views`.
- History: `undo`, `redo`, `jump`, `history`, `diff`, `cherry_pick`,
  `history_prune` (`topic:history`).
- Documents and catalog: `new_model`, `scene`, `set_model_name`,
  `save_prefab`, `load_prefab`, `catalog_list`, `catalog_describe`,
  `material_catalog`, `export_model` (`topic:scenes`, `topic:catalog`,
  `topic:export`, `topic:bsn`).
- **Engines**: `export_model` writes one model as `.gltf` / `.glb` (Bevy,
  Godot, anything), and a map (terrain, `prefab` instances, markers,
  colliders) as a level scene: `.bsn` for Bevy (jackdaw) or `.tscn` for
  Godot 4 (instanced glTFs, `Marker3D`s, components as metadata or
  scripted child nodes, physics bodies, terrain as a `HeightMapShape3D`;
  PNG textures, no `lods` / `ao`). In a Godot project
  (`project.godot`) export maps as `.tscn` and models as `.glb` / `.gltf`
  inside it. `topic:bsn` has both.
- Humans: `add_annotation` / `list_annotations` / `delete_annotation`,
  `set_camera`, `session_log` (`topic:session`).

**`apply_ops(ops, params?, expect?, ops_file?, doc?)`**: each item has a
`"type"`. `describe_op(kind)` has every field, so there is no need to
memorise them.

| type | fields (optional in []) |
|---|---|
| `primitive` | name, kind, size, [translation / position], [rotation], [frame], [round] (box / cylinder / wedge), [segments 4] |
| `extrude` | name, profile, height, [twist], [scale_top [1,1]], [slices], [round], [translation], [rotation] |
| `revolve` | name, profile, [degrees 360], [segments 48], [round], [translation], [rotation] |
| `sweep` | name, profile (one ring), path [[x,y,z]...], [closed], [round], [smooth], [samples], [twist], [scale_end] |
| `loft` | name, sections [{profile, y / at, [rotation], [scale]} or {profile, t}...], [path], [smooth], [samples], [steps], [round], [quads] (a subdiv cage) |
| `cage` | name, verts, faces (quads), [creases [[a, b, w]]], [mirror x] (a polygon cage `smooth subdiv` reads as quads) |
| `lsystem` | name, preset (tree / pine / bush / fern / weed / roots) or axiom + rules, [iterations], [angle], [length], [radius], [seed], [tropism], [leaf_size], [leaf_mode card / solid], [leaf_cross], ... (branching growth: wood in `name`, leaves in `name_leaves`, cards for an alpha-cutout leaf material) |
| `text` | name, text, size (capital height), [depth size/8], [font sans / bold / mono / path.ttf], [align center], [line_spacing], [letter_spacing], [translation], [rotation] |
| `terrain` | name, [size [60, 60]], [resolution 0.5], [height 6], [scale 20], [octaves 5], [style hills / ridges / dunes / mesa / flat], [seed], [base -1], [flatten], [paths], [edge_falloff], [terraces], [wind], [gain], [translation], [rotation] (noise ground, a closed slab) |
| `hull` | name, inputs |
| `blob` | name, inputs, [op union / subtract / intersect], [blend], [resolution] (smooth-blended solids) |
| `split` | name, input, plane {normal, point} or cutter, [keep], [gap] / [depth], [lift] (→ `name_above` / `name_below`, or `name_inside` / `name_outside`) |
| `scatter` | name, input (part, list = one picked per copy, `[a, b]` = a set moved as one, `"inst.*"`), on, count or density, [seed], [scale [min, max]], [rotate random_y], [align 0], [min_spacing], [max_slope], [sink], [region], [faces up], [weights], [near + near_distance 0.5], [edge_bias 0], [clump {count, radius}], [mask path / pad / not_path / not_pad], [height [min, max]]; or `decal` {pbr, size, [aspect 1], [depth], [roll random / none], [kind], [params]} instead of input |
| `round` | name, input, mode (fillet / chamfer / smooth), [radius], [segments], [edges all / top / bottom / vertical / horizontal or a list] (fillet) |
| `deform` | name, input, mode (displace / bend / twist / taper / material / offset), [axis y], [degrees], [toward], [scale_end], [power], [amount], [scale], [octaves], [seed], [noise], [detail], [pbr], [params] |
| `sdf` | name, shapes [{shape sphere / ellipsoid / box / capsule / cylinder / torus / chain / part / group, [op], [blend], ...}], [blend], [mirror x / z / xz], [warps], [resolution], [tris], [bake] (analytic distance-field modeling) |
| `skin` | name, chains [[[x, y, z, r] or [x, y, z, [rx, ry], roll?] or joint, ...], ...], [joints {name: [x, y, z, r]}], [smooth true], [blend], [mirror], [warps], [resolution], [tris], [bake] (a skeleton with radii as one smooth skin) |
| `body` | name, preset (biped / quadruped / bird / fish / serpent / car), [height], [head_ratio], [build], [leg_length], [tail], [snout], [joints {name: {scale, offset}}], [blend], [resolution], [tris], [bake], [rig true], [warps] (a parametric creature or car with a `<name>_rig` skeleton) |
| `silhouette` | name, side [[z, y]...], top [[x, z]...], [front [[x, y]...]], [smooth], [round], [blend], [from_reference {image, view, height / size}], [resolution], [tris], [bake], [translation], [rotation] (a solid carved from orthographic outlines) |
| `smooth` | name, input, mode (relax / remesh / subdiv), [iterations], [strength], [edge], [levels 2] or [tris], [crease], [symmetric] |
| `decimate` | name, input, ratio or tris, [bake], [resolution 1024], [cage], [crease 60], [symmetric x] (fewer triangles; bake the dense surface into normal + height maps) |
| `shell` | name, input, thickness, [open: face or [faces]], [method exact / offset (dense meshes)] (hollow a solid) |
| `carve` | [solid], cutters, [rooms [{name, footprint [[x, z]...], height, base}]], [slabs [{footprint [[[x, z]...]...], top, thickness 0.3}]], [wall 0.3], [keep inner / outer / both], [merge true], [split_by none / facing / cutter / both], [markers], [collider none / mesh / blocks] (interiors by subtraction: only the faces looking into the rooms, as one-sided sheets) |
| `stairs` | name, from [x, y, z], toward (+x / -x / +z / -z or degrees), rise, [width 1.2], [steps], [run 0.3], [collider none / blocks] (a flight as one closed solid, a `block` marker per step) |
| `railing` | name, path [[x, y, z]...], [height 1], [post_spacing 1.5], [thickness 0.06], [mid_rail true], [collider none / blocks] (rails and posts as one part) |
| `garment` | name, on, [preset], [region {zones, arms, legs, neck, above, below, planes, boxes}], [offset 0.006], [thickness 0.003], [ease], [hem {flare, length}], [fit tight / regular / loose / baggy], [skirt {top, hem, length, profile, folds, flare}], [hide_skin true], [bind] (binds itself) (clothing cut from a body's surface) |
| `panel_line` | name, input, view, points [[u, v]...], width, depth, [closed], [smooth], [fill], [cut true] (a line drawn in a view, cut into the surface as a groove; `fill`: an outline as a cutter) |
| `panel` | name, input, face, [rows 1], [cols 1], [inset / border 0.01], [depth 0.005], [gap / seam 0.01] (recessed panels in a face) |
| `group` | name, inputs |
| `transform` | name, input, [translation], [rotation], [scale], [relative], [pivot] |
| `move` | [name], parts (names / `*` globs; `["*"]` = all), [translation], [rotation] or [axis + degrees], [pivot: point or anchor] |
| `place` | [name], parts, on, [at], [gap], [toward down], [each] (drop onto `on`, or mount against a wall) |
| `boolean` | name, op (union / difference / intersection), a, b |
| `radial_array` | name, input, count, [center], [degrees 360], [start_degrees], [axis y], [include_source true] |
| `linear_array` | name, input, count, offset, [local] |
| `path_array` | name, input, path, count or spacing, [smooth], [closed], [align true], [roll_step], [on], [follow upright / slope / normal], [between] |
| `mirror` | name, input, axis, [offset] |
| `material` | name, [input = name], [color / base_color], [roughness], [metalness], [pbr], [bevel], [macro] |
| `prefab` | name, prefab (catalog entry), [translation], [rotation], [scale], [components], [variant] |
| `hide_part` / `show_part` | part (`*` matches anything: `"cut*"` hides every cutter, `"*_raw"` every raw blank) |
| `rename` | part, to |
| `delete` | name (an op output, or a baked part; its own in-place hide/material/transform edits go with it) |
| `tri_mesh` | name, verts, indices, [normals], [uvs] (baked raw triangles) |
| `collider` | name, parts (globs), shape (box / hull / mesh / none), [body static / dynamic / kinematic], [sensor], [hidden] |
| `marker` | name, kind, [translation], [yaw], [size], [data], [attach], [preview], [components] |
| `variant` | name, [materials {part or glob: material item fields}], [style {token patch}], [show], [hide] (a cosmetic skin or an outfit; geometry unchanged) |
| `light` | name, kind (point / spot / directional), [color], [intensity], [range], [translation], [direction], [inner_degrees], [outer_degrees], [shadows], [attach] |
| `decal` | name, pbr (a `decal`-domain material), size [w, h] or [w, h, depth], [on], [at], [toward down], [translation], [direction], [roll], [attach], [kind clustered / forward], [params] |
| `skeleton` | name, bones {name: {head, [tail], [parent], [radius]}} (points or anchors), [mirror x] or [from: lsystem / skin op, bone_length, max_depth, max_bones] |
| `bind` | name, skeleton, parts, [bone] (rigid) or [mode smooth / auto], [blend], [paint] |
| `clip` | name, skeleton, duration, tracks {bone: {rotate / translate / scale: keys, wave or {euler}}}, [mirror {bone: {from, phase}}], [ik], [poses [[t, pose]]], [over clip], [additive], [weight], [bones], [root_motion], [events [[t, name, data?]]], [loop true], [interp smooth] |
| `pose` | name, skeleton, bones {bone: {rotate, translate, scale}}, [mirror], [ik {end bone: {target, pole, chain, rotate}}] (a static pose clips key whole) |
| `mocap` | name, skeleton, file (a project BVH) or bvh (inline text), [map "auto" / {bone: joint} / {"only": {...}}], [range [t0, t1]], [fps 30], [loop false / true / "blend" + blend 0.25], [scale], [root_motion "in_place" / true / {axes}], [feet {plant, contact_height 0.03, contact_speed 0.15}], [hands], [mirror], [speed 1], [facing deg / "auto"], [interp linear] (motion capture retargeted into an ordinary clip) |
| `sway` | name, skeleton, [kind wind / spring], [over clip], [bones], [duration 4], [fps 30], wind: [direction], [strength], [gust], [flutter], [frequency], [seed]; spring: [stiffness], [damping], [gravity] |
| `uvs` | part, projection (box / planar / cylindrical / spherical / metric = box in meters / face = one upright image per side, alias label; planar maps along the thinnest axis) |
| `repeat` | var, count or values, items (a loop, expanded before anything else) |

## Rules that hold everywhere

The full list, with the fine print, is `topic:apply-ops`.

- **Upserts**: an item with the same name *and type* replaces the old one
  and re-derives (the tweak path). Order doesn't matter: evaluation is
  dependency-ordered, and part edits (hide/show, rename, uvs, move/place)
  run after every op, in the order written.
- **Batches are atomic**: all items are parsed first (errors name the item
  and field, unknown fields are rejected), and any failure rolls the whole
  call back. `expect` runs `verify` checks after the batch (a failure makes
  the call an error, but the batch stays applied).
- **Forms**: sizes as a number, a short array or an object; colours
  `[r, g, b]`, 0..255, `"#rgb"`; rotations as a quat, `{axis, degrees}` or
  Euler degrees `{x, y, z}`.
- **Params and loops**: `params: {"h": 2}` with `"$h"` / `"=$h / 2"`
  anywhere a number goes; a later param change re-derives every reader.
  `repeat` copies items per value (`{r}` in strings, `$r` in expressions).
- **Don't place by arithmetic**: `place` drops parts onto a surface (or
  against a wall); `move` pivots and `at` take anchors
  (`"screen:bottom-front"`); `frame` builds in another part's coordinates.
  These follow their targets when params change.
- **Closed solids**: booleans (Manifold) need closed inputs; disc / plane /
  washer are open sheets. Hide inputs (blanks, cutters, array and scatter
  sources); a visible input fails `collisions` with a hint.
- **Materials follow lineage** (copies and cuts of X inherit X's material).
  Every shipped model passes `verify materials: "pbr"` unless the user asks
  for flat colours (`"flat": true`). Shape first, then materials, then rig.
- **Nothing real has knife edges**: `round` on box / cylinder / wedge
  primitives (a few mm), `round` fillets, `bevel` on materials.
- **Prefer numbers to eyeballing**: `verify` (grounded, collisions,
  validate, walkable, materials, ...) and `inspect`, then one render.

## Texturing by default

Agents tend to stop at grey or flat-coloured geometry. Don't: unless the
user asks for a blockout or plain colours (`flat: true`), every model
ships textured, planned together with the shape.

1. **Pick a domain per part.** Tile (the default) for anything that
   repeats or is reused across models (wood, brushed metal, painted steel,
   fabric, stone, concrete): metric uvs map it at real size and kits share
   one texture. Object for hero parts that need wear following their own
   shape: `mesh_ao` / `mesh_curvature` (dirt in creases, worn edges),
   `gradient` fades, 3D noise with no seams. Card / decal for cutouts.
2. **Put artwork where it belongs.** Badges, labels, gauges, panel lines
   and markings: an `svg` (or `text`) node on a `project {face, at}` coords
   node, masked by `project.facing`, in the part's object material; stains,
   cracks, graffiti: `decal` items or a decal `scatter`.
3. **Layer, don't paint one tone.** Base + `over` layers (rust over paint,
   dirt where `mesh_curvature.concave`, wear where `.convex`), roughness
   variation, a `bevel` finish on hard edges, `macro` on big surfaces.
4. **Check it**: `material_view` per material, one `agent_views` render
   of the model, and `verify materials: "pbr"` in the batch's `expect`.

## Interior workflow

Rooms, corridors, bunkers, ship decks: carve them out instead of building
walls (`describe_op("carve")` has the details).

1. **Cutters, not walls**: one closed box (or `extrude` of a footprint, or a
   `rooms` entry) per room, hall and doorway, at real size (rooms 2.4-3 m
   high, doors 1 x 2.1 m through the wall, overlapping both rooms by a few
   cm). Hide them.
2. **Carve**: `carve` with `split_by: "facing"` (or `"both"` when rooms
   have different looks), `markers: true`, `collider: "mesh"`. Omit `solid`
   unless the building has a shape.
3. **Materials**: give each cutter its material first (split by cutter), or
   `material` the outputs by pattern; floor / wall / ceiling tile materials
   at real scale, `macro` on big walls.
4. **Check**: `verify` with `walkable` along the route between spawn and
   goal (and `grounded`, `validate`); `inspect measure` for headroom.
5. **Dress**: `scatter` clutter with `near` a wall and `edge_bias`, decals,
   `light` markers per room (the `room` markers have each room's centre and
   box), prefab props `place`d on the floor sheets.
6. **Export** `.gltf` with `optimize: "game"` (add `ao` for baked occlusion:
   merged quads make the atlas small), or a `.bsn` (Godot: `.tscn`, no `ao`) map: the room / opening
   markers and colliders come along.

## Organic workflow

Creatures, characters, vehicles and other sculpted forms (the per-op
details: `describe_op` of `body`, `sdf`, `skin`, `silhouette`, and
`topic:sections`):

1. **Start from a preset or outlines**: `body` (biped / quadruped / bird /
   fish / serpent / car, tuned with `height`, `build`, `head_ratio`,
   per-joint `{scale, offset}`), or `silhouette` (side / top / front
   outlines, `smooth: true`; also an sdf shape, so arches, sockets and
   extra limbs join by smooth booleans; `from_reference` traces a PNG), or
   `sdf` / `skin` with oval sections when neither fits.
2. **Compare with references**: `agent_views` with `reference: [{image,
   view: "front"}, {..."right"}, {..."top"}]` (up to 3 views in practice)
   and read the IoU and the **band deviations** (`front: top 0-20%: model
   14% wider (+3.1 cm)`): each names where and by how much to change.
3. **Fix locally with warps** on the same item (`warps: [{mode: inflate /
   push / pinch / flatten / smooth, at: a joint name or point, radius,
   amount}]`) or joint overrides, not by adding shapes; re-check the bands.
4. **Check the surface and proportions**: `agent_views style:
   "curvature"` (lumps, pinches, creases), `inspect proportions` (bone-chain
   ratios: biped head / height ~0.133), `verify self_intersections` /
   `thickness`.
5. **Make it light last**: `tris: N` on the item (plus `bake: true` to keep
   the dense detail in normal / height maps), then rig and animate.

Level-set results (`sdf`, `skin`, `body`, `silhouette`, `blob`, `smooth`
remesh) are finished automatically: vertices are projected onto the field,
sharp joins are reconstructed as straight runs of mesh edges and normals
come from the field gradient. A join meant to be soft needs a `blend`, not
a mesher trick. `decimate` / `tris` keeps those creases (feature-edge
quadrics); a `ragged crease` warning means one came out zigzag anyway.

`examples/creature.rs` (body + warps + tris) and `examples/car_body.rs`
(silhouette shape + arch subtractions + wheels) run this end to end.

## Rigging and animation

Rig after the shape and materials are done. `skeleton` (or the rig a
`body` brings), `bind`, then `clip` / `pose` / `mocap` / `sway`; check with
`verify animation` and filmstrips (`agent_views` with `clip` and `times`).
Hand-keyed human motion comes out stiff: prefer `mocap` or the generated
walks in `examples/`. Guides: `topic:animation`, `describe_op("clip")`,
`topic:motion-checks`, `topic:human`, `topic:runtime` (the Bevy plugin).

## Doc map

| file | covers |
|---|---|
| `docs/ops/<type>.md` | one apply_ops item type (`describe_op(type)`) |
| `docs/topics/apply-ops.md` | semantics: upserts, ordering, params, ops_file, results, caching, history snapshots, save format, scene_summary warnings |
| `docs/topics/profiles.md`, `sections.md` | 2D profiles, arcs, SVG, smooth paths; oval / superellipse sections, `tris` / `bake` |
| `docs/topics/verify.md`, `inspect.md`, `views.md` | checks, numeric senses, renders and their image cost |
| `docs/topics/materials.md`, `style.md` | procedural material graphs, style tokens |
| `docs/topics/animation.md`, `motion-checks.md`, `human.md`, `runtime.md` | rigs, motion checks, the human template, `modeler_bevy` |
| `docs/topics/export.md`, `game-data.md`, `bsn.md` | game export, colliders and markers, `.bsn` / Godot `.tscn` level scenes |
| `docs/topics/scenes.md`, `catalog.md`, `history.md`, `session.md`, `daemon.md` | open documents, catalog, history tools, annotations and feedback, the daemon |
| `docs/preview.md` | the preview window (human side): workbench layout, design system, panes |
| `docs/dev.md` | server internals: multi-agent calls, off-thread slow calls, autosave lock, restart flow |

## Dev notes / gotchas

- **Several agents share one daemon**: calls run concurrently, edits apply
  one at a time on the app thread, a per-document lock keeps one document's
  `apply_ops` in order, and authorship / undo guards are per client. Slow
  calls warm the op cache on a worker snapshot, then commit on the app
  thread. New slow read-only tools use `run_detached_json`; slow mutating
  ones the warm-then-commit pattern (`docs/dev.md`).
- The daemon keeps the server's stderr in `<catalog dir>/daemon.log`; its
  last line is `modeler: exit: <reason>`.
- rmcp 1.8 runs a tokio current-thread runtime on a spawned thread; the Bevy
  app owns the main thread (winit on macOS). Bridge = tokio mpsc + oneshot.
- `--headless` uses MinimalPlugins — don't add ScheduleRunnerPlugin again.
  `--offscreen` is DefaultPlugins with no primary window, driven by
  ScheduleRunnerPlugin at 60 Hz.
- Lints: `clippy::unwrap_used` and `expect_err` are denied — use
  `let Ok(..) = .. else`, `unwrap_or_else(|e| panic!("{e}"))` in tests, and
  return ids from APIs instead of unwrapping options. clippy denies
  `chunks_exact(3)` with a const chunk — use `as_chunks::<3>().0`.
- f32: never assert derived coordinates with `round()` (0.49999997 rounds to
  0) — compare with a 1e-5 tolerance.
- Bevy 0.20: `bevy::shape` has `Circle` / `Annulus` (`inner_circle` /
  `outer_circle`); `MeshRayCast` borrows `Assets<Meshes>` (defer pin
  spawns); `AccumulatedMouseScroll` for the wheel; winit sends `Key::Space`.
  Never give a UI node a margin that can exceed its parent's width (Bevy's
  border-radius clamp panics on a negative size).
- In tests that hit the catalog, set `FRIENDSLOP_DATA_DIR` to a tempdir.

## Conventions

- New ops: add the variant in `lib.rs` (`Op`), an eval arm + `output`/`kind`/
  `inputs`/`describe` arms in `graph.rs`, and in `src/spec.rs` a `*Spec`
  struct (`deny_unknown_fields`), an `OpSpec` variant, an `OP_TYPES` entry,
  a `parse_item` arm, an `example`, a `describe_type` schema arm and an
  `op_guide` entry pointing at its new `docs/ops/<type>.md` (tests enforce
  the last three). No new MCP tool: `apply_ops` carries it. Validate op
  params BEFORE the input lookup so errors report the right cause.
- **Reference prose goes in `docs/`, not here**: an op's details in its
  `docs/ops` file, cross-cutting ones in a `docs/topics` file (add it to
  `TOPICS` / `topic()` in `src/spec.rs`). This file has a size budget
  (`agents_md_stays_small`), because it loads into every session.
- Keep the tool count small: every tool's schema costs context on every
  turn. New capabilities go into `apply_ops` item types or `verify` fields.
- Examples in `examples/` double as executable documentation of the intended
  workflow — and as the fallback for authoring procedural prefabs when the
  agent's MCP tool list predates a tool.
- Keep `README.md` for humans, this file for agents.
