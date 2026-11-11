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
done. Typical first calls: `scene_summary` (also shows the autosaved session,
if any), `catalog_list`, then `new_model` + `apply_ops` to build. If your
session's MCP tool list has no `mcp__modeler__*` tools, this driver is the
documented fallback — it speaks the same tool surface over stdio. The daemon
keeps the preview window open; check it with `screenshot` (aim first with
`set_camera`) and verify numerically with the Verification tools below.

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

## MCP tool surface

Modeling (all op tools are **upserts**: re-issuing an op with the same output
name *and kind* replaces it in place and re-derives — that is the parameter
tweak path):

- `add_primitive(kind, size, translation, name)` / `add_tri_mesh(...)` — baked
  parts, no graph. Fine for one-offs; prefer `op_primitive` for anything you
  may tweak.
- **Booleans** nudge the second operand ~2 mm when one of its faces shares a
  plane with a face of the first (csgrs drops faces on coplanar-but-disjoint
  planes, e.g. two equal-depth cutters). `scene_summary` warns when a visible
  boolean output has open edges that enclose real missing area (hairline seams
  don't trigger it). Booleans output welded meshes (1e-4 grid), but curved cuts can still
  carry seam edges — `validate` reports them honestly; they render fine in
  practice. Curved-on-curved (cylinder−cylinder, torus anything) can produce
  corrupted noise — compose or fake instead (the catalog's mug does).
  Boolean math comes from csgrs, which only exports polygon soup, so the
  weld is the glue; don't expect guaranteed manifoldness.
- `op_primitive(name, kind, size, translation, rotation?)` — `size` is
  **always exactly `[f32; 3]`**: `box`/`plane`/`wedge` (full extents),
  `sphere` (**radius = size[0]; [1]/[2] ignored** — flatten with a transform's
  scale), `cylinder`/`cone`/`capsule` (size[0] = radius, size[1] = height),
  `torus` (ring, tube; needs tube < ring), `disc`/`washer` (flat, in XY
  plane; washer = inner < outer radius). `rotation` is an optional quaternion
  `[x, y, z, w]`, so rotated primitives need no separate transform.
- `op_transform(name, input, translation?, rotation? [quat xyzw], scale?)` —
  place/rotate/scale an earlier part into a new output. The values are
  **absolute, not additive**: a part at y=0.375 transformed with
  `translation [0.4, 0, 0]` lands at y=0. Restate the template's full
  position/rotation in every copy.
- `op_boolean(name, op, a, b)` — `union`/`difference`/`intersection`; bakes
  transforms into verts. Panics inside csgrs on degenerate triangles, so keep
  inputs sane. Errors (e.g. no overlap) roll the graph back — it never stays
  broken.
- `op_radial_array(name, input, count, center, degrees, start_degrees,
  axis?, include_source?)` — ring of copies about `axis` (`"y"` default; `"x"`/`"z"` make
  horizontal rings — windmill blades, fan spokes — no manual rotation
  needed). `include_source: true` is the easy mode: the source is copy 0 and `count`
  items spread evenly over `degrees` (copies `name_1..name_{count-1}`).
  Otherwise **the source part stays at its own spot**; with
  `start_degrees = degrees/count` and `count-1` copies the source fills the
  0° slot evenly with no overlap. Outputs `name_0..{n-1}`.
- `op_linear_array(name, input, count, offset)` — line of copies. **The source
  is copy 0**; outputs `name_1..{n-1}` at `input + i*offset`. Nothing overlaps
  the input.
- `op_mirror(name, input, axis, offset)` — reflect across the plane
  `axis = offset[axis]`. **Baked**: verts go to world space, are reflected,
  winding flips, normals rebuilt (negative scale would render inside-out).
- `op_material(name, input, base_color?, roughness?, metalness?)` — color in
  place or into a new output. `base_color` is **always RGBA `[f32; 4]`** (in
  `op_material` and in `apply_ops` material specs alike); a 3-component RGB
  fails deserialization.
- **disc/washer/plane are double-sided** (a reversed twin of every triangle is
  appended, so neither orientation culls), flat in the XY plane (vertical!).
  Laying one flat still takes a −90° X rotation (`[-0.707, 0, 0, 0.707]`) so
  it lies horizontally. They are two coincident sheets, so avoid placing
  another surface exactly on one (z-fighting).
- `op_delete(name)` / `ops` — remove a step / list the graph one line per op
  **with full parameters** (sizes, translations, colors — enough to tweak
  without remembering what you sent). `op_get(name)` dumps a single op.
- `set_model_name(name)` — names the whole model; used as the prefab entry
  name on `save_prefab`, so set it before saving.
- `apply_ops(ops)` — a whole batch in one call: array of objects, each tagged
  `"type"` (`primitive`, `transform`, `boolean`, `radial_array`,
  `linear_array`, `mirror`, `material` plus the baked edits `hide_part`
  `{"part"}`, `show_part` `{"part"}`, `rename` `{"part", "to"}`) with that
  op's usual parameters. Applied in order with the same upsert/rollback
  semantics; stops at the first error with per-item context (earlier items
  stay). The result ends with the scene summary — this is the fast path for
  building a model.
- `new_model(name)` — fresh empty session model without restarting the
  server; the old model is recorded in history, so one `undo` restores it.
- `hide_part(part)` / `show_part(part)` — the argument is named `part`. Hidden
  parts stay in the model and
  keep deriving, but the preview and glTF export skip them. The clean way to
  keep boolean intermediates out of an export. On op-driven models the edit
  is recorded as a graph op (`Visibility`), so it survives later tweaks; the
  same goes for `rename_part`, `set_material` and `transform_part` (emitted as
  `Rename`/`Material`/`Transform` ops). Only parts that no op produces (baked
  parts), `set_uvs` and `delete_part` stay baked and are lost on re-derivation.

Verification (all deterministic — prefer these over eyeballing renders,
then screenshot last as the final once-over):

- `validate` — per-part mesh checks without a renderer: edge manifoldness
  (welded by position) and winding via signed volume. Catches inverted
  normals and torn geometry that a screenshot only hints at.
- `probe_ray(origin, dir, all_hits?)` — cast a ray through the live scene and report the
  first part hit (name, distance, point) or that it passes clean through;
  `all_hits: true` lists every part along the ray, nearest first (what is
  behind an opening).
  The numeric check for openings and occlusion: cast through a window hole
  to prove it is open, cast at a door fill to prove it is solid. Windowed
  preview only.
- `check_rebuild` — re-derives the graph into a scratch copy and diffs the
  geometry against the cached parts; fails on a stale cache or
  non-deterministic op (baked material/visibility edits are not compared —
  a refresh legitimately resets those).
- `expect_parts(parts)` — exact part-list assertion over **visible parts
  only** (the argument is named `parts`); hidden templates are not counted. (order-insensitive),
  reporting missing and unexpected names. The deterministic replacement for
  counting parts in a screenshot; use it after arrays/booleans to catch
  missing copies, duplicated radial sources, or stray intermediates.
- `expect_bounds(min, max, tol)` — numeric world-bounds assertion over
  visible parts only, per axis (`null` skips). This is "sits on the ground"
  (min y ≈ 0), "centered" (symmetric x/z), "roof peaks at ~2.2" (max y) as
  a checkable call instead of a squint at a render.
- `set_camera(yaw, pitch, dist)` — point the human's orbit camera (degrees,
  metres) before a `screenshot`. Windowed preview only.
- `agent_shot(view, width?, height?, focus?)` / `agent_views([views], ...)` — render from
  the agent's own offscreen camera, independent of the human's window: six
  orthographic views (`front`/`back`/`left`/`right`/`top`/`bottom`, true
  proportions, auto-framed to the bounds) plus `iso` (perspective
  three-quarter). `focus: [part names]` frames just those parts. Returns PNGs; `agent_views(["front", "right", "top"])` is
  the three-pane check, `["iso"]` the looks-right shot.

State:

- `undo` / `redo` / `jump(id)` / `history` — the tree. `history` prints one
  line per node (id, parent, label, version, `<-` cursor); jump by id, then
  the next edit branches. Use jump to try an alternative and keep both.
- `scene_summary` — call this FIRST and after anything confusing: parts,
  bounds, ops, annotations, plus deterministic **warnings** (see below).
- `set_material` / `transform_part` / `delete_part` / `rename_part` / `set_uvs`
  — baked-part edits (record history like everything else).
- `add_annotation(part, text, face?, vertex?, position?)` /
  `list_annotations` / `delete_annotation(id)` — human comments pinned to
  parts. If the human clicked a spot in the preview, omit all optional args
  and the pending pin is consumed. Annotations ride in prefabs.
- `save_prefab(description, tags, prompt)` — all three required, including
  `prompt` — / `load_prefab(name)` /
  `catalog_list` / `catalog_describe` — persistence.
- `save_prefab` reports whether it overwrote an existing catalog entry
  ("saved `x` — overwrote an existing catalog entry") — if you didn't mean
  to, the old JSON is gone; check `catalog_list` and the model name first.
- `session_log(limit?)` — the recent per-call record of every tool call (tool, ok/error, ms).
  Self-audit a build: what failed, what was slow.
- **Session autosave**: every successful mutation also writes the whole
  session model (parts, ops, history tree, annotations) to `session.json`
  in the catalog dir, and a server started without `--model` restores it.
  Unnamed WIP therefore survives restarts and crashes without any explicit
  save; `save_prefab` is only for naming and indexing it.
- `export_model(path, "glb"|"gltf")`, `screenshot` — output. Render-dependent
  tools (`screenshot`, `agent_shot(s)`, `probe_ray`, `set_camera`) time out
  after 15s with a retry hint instead of hanging the server. **Always
  screenshot and actually look at the image** after building something; the
  scene data can be right while the render is wrong.

## Semantics worth knowing

- **In-place ops**: `op_material("petal", "petal")` (output == input) is
  legal — upsert never replaces an op the new op reads from (`graph::kind` +
  `graph::inputs`). Repeated in-place ops accumulate; last wins.
- **Op results are memoized** (`graph::evaluate`): a process-wide cache keyed by
  op parameters + input content hashes, so tweaking one op re-runs only it and
  its dependents. `check_rebuild` bypasses it (`evaluate_uncached`).
- **Op evaluation is dependency-ordered.** Each op runs after the op
  producing its inputs; where that is already consistent, written order is
  preserved. This heals graphs where an upsert left an op positioned before
  an input it gained later — no naming tricks needed. A true cycle fails with
  "ops form a dependency cycle involving ...". Ops can only reference op
  outputs, never baked parts.
- **Op-driven snapshots store no parts** (`History::record` strips them when
  they derive from ops); undo/redo/jump re-derive via the eval cache.
  `history_prune(keep)` bounds long sessions (keeps newest N, cursor, root,
  branch points; renumbers ids).
- **History snapshots are history-less**: each node stores a model with an
  empty tree (the live tree on the session model is the single source of
  truth); undo/redo/jump graft the live tree onto whatever they land on.
- **Jump restores the snapshot's op list exactly.** After `jump`, every op
  created after that node no longer exists — re-issue all of them in the new
  branch. "no part `x` earlier in the graph" right after a jump means this.
- **Save format is versioned** (`Model::format_version`, `FORMAT_VERSION`):
  old files (no field) read as 0 and load; files from a newer build are
  rejected with a clear error. Bump it when the serialized shape changes.
- **Prefab format is non-destructive**: when every part re-derives from ops
  (`parts_derive_from_ops()`), `parts` are NOT written — just ops, history
  tree, annotations — and load re-evaluates. Mixed baked+ops models keep
  their parts. Writes are `.tmp` + rename. Old/plain prefabs load unchanged.
- **Primitive sphere radius gotcha** (above) and **winding**: never mirror via
  negative scale; use `op_mirror`.
- **Material before copies.** Array/mirror/transform ops clone their input at
  eval time, so a material op that runs *after* the copy colors only the
  source — the copies stay default-gray. Always `op_material` before the ops
  that duplicate the part. `scene_summary` warns when it sees this ordering.

`scene_summary` appends a warnings section for exactly the traps above:
uncolored copies (material after the copying op), full-ring radial arrays
whose copy 0 overlaps the source, visible parts still wearing the default
material, parts with identical world bounds (z-fighting), and boolean
volume sanity (a difference that didn't shrink its input = the cutter
misses; a union whose volume exactly sums its inputs = no overlap). Hidden
parts are excluded from all of these, so template clutter doesn't drown
real signals. An empty warnings list means none of these fired — checking
it is cheaper than eyeballing a render.

## Preview window (human side)

Left/middle/right-drag = orbit/pan, scroll = zoom, arrows + PageUp/Down too.
Auto-drift stops once the human steers; a model change re-frames the camera
around true world bounds (bounds include part transforms). **G** toggles the
read-only ops + history panel. Click a part then type to annotate
(Enter commits, Esc cancels; Backspace deletes; spaces work).

## Dev notes / gotchas

- Agents driving the server over stdio (frozen MCP tool list) should run
  `python3 mcp_driver.py --daemon --windowed` once in the background and then
  `python3 mcp_driver.py --connect --calls <file>` per batch: one long-lived
  server means the preview window stays open and state is live between
  invocations (autosave covers even a daemon restart). `{"name": "quit"}`
  stops it. The default throwaway mode spawns and kills a server per
  invocation — window flashes open/closed, state restored from autosave.

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
  `inputs`/`describe` arms in `graph.rs`, a session fn in `commands.rs`
  (goes through `apply_op`, which handles upsert/rollback/history), args
  struct + `#[tool]` fn in `src/bin/modeler/` (`server/` = MCP, `preview/` = Bevy), and a test. Validate op
  params BEFORE the input lookup so errors report the right cause.
- Examples in `examples/` double as executable documentation of the intended
  workflow — and as the fallback for authoring procedural prefabs when the
  agent's MCP tool list predates a tool.
- Keep `README.md` for humans, this file for agents.
