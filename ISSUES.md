# ISSUES.md — review pass (2026-11-10)

A quick review after the agent-driven build-out. Ordered by priority. Each item
says what's wrong, where it is and how to fix it. Run `cargo test` and
`cargo clippy --all-targets -- -D warnings` after each item, and add a
regression test for every bug fix.

## A. Bugs

### A1. Array outputs (`name_0..n`) are invisible to dependency resolution
`graph::output()` returns only the op's `name`. `RadialArray` actually emits
`{name}_0..{name}_{count-1}`, and `LinearArray` emits `{name}_1..`. In
`graph::eval_order`, an op that reads `ring_2` finds no producer, so it gets no
dependency edge. If it sits earlier in the list (after an upsert, or written out
of order), it runs before the array and fails with "no part `ring_2`". This
breaks the "dependency-ordered eval heals ordering" promise. The same root cause
breaks two more things:
- `op_delete("ring_2")` says "no op with output" — fine — but `op_delete("ring")`
  silently orphans ops that read `ring_*`. The error only shows up later.
- The upsert match in `Session::apply_op` uses `output()`, so a primitive named
  `ring_0` and a radial array named `ring` can both claim the part `ring_0`.
  Whichever runs last wins, without any warning.

**Fix:** add `pub fn outputs(op: &Op) -> Vec<String>` to `graph.rs`. It returns
every part name an op writes: the array names expanded by `count`, with the
linear array starting at 1. Use it in `eval_order` (producer lookup), in the
lints and in `op_delete` (report dependents). Also reject an op in `apply_op`
when its outputs collide with another op's outputs of a different kind. Keep
`output()` as the op's *identity* key for upsert. Tests: an op that reads
`arr_1` is listed before the array and still evaluates; a colliding name is
rejected.

### A2. Undo/redo/jump call `refresh()`, which wipes baked edits
`Session::undo/redo/jump` (`commands.rs` ~818–846) always call
`model.refresh()`. Take a snapshot recorded after `hide_part`, `set_material`,
`rename_part` or `transform_part` on an op-driven model: refresh re-derives the
parts and silently drops the edit. Undoing *to* a "hide" state therefore shows
the part again. It's also slow, because every undo re-runs every boolean.

**Fix:** refresh only when the parts are missing, i.e.
`model.procedural() && model.parts.is_empty()` (the snapshot was serialized
without parts). Otherwise trust the snapshot's cached parts. The same rule
applies on prefab and session load (`load_prefab`, `load_session_state` callers
in `src/bin/modeler.rs`). Test: hide a part, make another edit, undo, and the
part is still hidden.

### A3. Autosave runs after every *successful* call, including read-only ones
`Tools::run` (`src/bin/modeler.rs` ~1220) autosaves whenever `reply.is_ok()`.
That includes `scene_summary`, `validate`, `ops`, `history` and the rest. Each
autosave pretty-prints the whole model **plus every history snapshot**. A mixed
or baked model has full meshes in every snapshot, so the cost is
O(history × mesh) per call, and it grows without bound.

**Fix:** autosave only when `session.model.version` (or the history length,
since annotations don't bump the version) changed during the call. Capture it
before `f(session)` and compare after. Use compact `to_string` for
`session.json`, not pretty. Optionally debounce it (save at most every N ms,
plus once on quit).

### A4. A csgrs panic can kill the server
AGENTS.md says csgrs "panics on degenerate triangles", but there is no
`catch_unwind` anywhere in `src/`. A panic on the Bevy main thread takes down
the preview and the MCP server.

**Fix:** wrap `run_op(op, &csg_a, &csg_b)` in `csg::boolean` (`csg.rs` ~175)
with `std::panic::catch_unwind(AssertUnwindSafe(..))` and turn the panic into
`Err("csg panicked on {op}({a},{b}): degenerate input")`. Better still, drop
zero-area or NaN triangles in `part_to_csg` before handing them over.

### A5. `part_centroid` ignores rotation and scale
`Model::part_centroid` (`lib.rs`) adds the translation to the mean of the
*local* verts. Any rotated or scaled part therefore gets its annotation pinned
in the wrong place. **Fix:** transform each vert the way `Part::bounds()` does,
or reuse a shared `Part::world_vert(i)` helper. Mirror and the lints should use
the same helper.

### A6. `catalog.json` write is not atomic
`save_catalog` uses a plain `fs::write`. The prefab and session saves use
tmp + rename. A crash mid-write corrupts the index, and `load_catalog` then
errors out on every later save. **Fix:** write to a tmp file and rename, and
factor out an `atomic_write(path, bytes)` helper for all three call sites.

### A7. `save_prefab` reads the catalog twice (racy, wasteful)
It loads the catalog once for `overwrote` and again before it upserts. **Fix:**
load it once at the top, and derive `overwrote` from that same value.

### A8. Docs disagree with each other
- STATUS.md says `disc/washer/plane` are **double-sided**. AGENTS.md says
  **single-sided** (and tells agents to rotate −90° X or they cull to
  invisible). Check `primitives.rs` and the Bevy material (`cull_mode`), then
  fix whichever doc is wrong, plus the server `instructions` string in
  `modeler.rs` (~2529).
- Test counts: AGENTS.md says 58 + 4, STATUS.md says 63 + 4. Remove hard-coded
  counts entirely; they rot.
- STATUS.md says 46 tools; `modeler.rs` has 48 `#[tool]` attributes. Same fix.
- `Cargo.toml` sets `unwrap_used = "warn"`, but AGENTS.md says it is
  "denied". Make it `deny` to match the stated policy.

## B. Structural issues (larger, worth redoing)

### B1. History stores full model clones — make it op-level or structurally shared
Every edit pushes a complete `Model` clone, parts included, into `History`.
Memory and autosave size grow as edits × mesh size. A 200-edit session on a
boolean-heavy model can easily hold hundreds of MB, and serializing it on every
call (A3) makes this worse.

**Fix options, in order of payoff:**
1. Store `parts` as `Arc<Part>` (or `Arc<[Part]>`) in `Model`, so unchanged
   parts are shared between snapshots. serde needs `rc` feature handling, or a
   custom (de)serialize.
2. For procedural models, snapshots store only `ops` + annotations and
   re-derive on navigation. Put an eval cache in front (see B2) so this is
   cheap.
3. Add a cap or prune: keep the newest N nodes plus every node that is a
   branch point, with a `history_prune` tool.

### B2. No evaluation cache — every edit re-runs the whole graph
`Model::refresh` → `graph::evaluate` rebuilds every part from scratch on every
op upsert. Booleans dominate (STATUS mentions csgrs repairs "taking minutes").
Tweaking a material at the end of the graph re-runs every boolean upstream.

**Fix:** memoize per op. Key each op by a hash of (op params + its input
parts' hashes) and keep a `HashMap<u64, Vec<Part>>` on the `Session`, not
serialized. `evaluate` looks up the key before it runs an op. This also makes
undo/redo/jump re-derivation (A2, B1 option 2) effectively free. `Op` needs a
stable hash: hash `serde_json::to_vec(op)`, or derive `Hash` on a bit-cast of
the floats.

### B3. Baked edits on op-driven models are a trap — move them into the graph
`hide_part`, `rename_part`, `set_material`, `transform_part` and `set_uvs` on
an op-derived model are lost on the next op tweak. This is documented, but it
is a sharp edge agents keep hitting (the shakedown queued "graph-level rename").

**Fix:** add `Op::Visibility { name/input, visible }` and `Op::Rename` (or
better, an `output_alias`). Then make the baked tools *emit ops* when
`model.procedural()`, and keep the baked path only for fully baked models.
`apply_ops` `hide_part`/`rename` items then become ordinary ops, and
`derived_from_ops`/`mark_baked` mostly disappears.

### B4. Ops are stringly typed
`kind: String`, `op: String`, `axis: String` are parsed at eval time with
ad-hoc `match` arms that repeat across `graph.rs`, `primitives.rs` and
`csg.rs`. A typo is only caught at evaluation, and the mirror `describe` does
`"xyz".find(axis).unwrap_or(0)`.

**Fix:** use `enum PrimitiveKind`, `enum BoolOp` and `enum Axis` with
`#[serde(rename_all = "snake_case")]`. serde then rejects bad values at the
MCP boundary with a clear message, and the eval code loses its error arms. Keep
the JSON wire format identical, so old prefabs still load (add a test that
loads an existing catalog prefab).

### B5. `src/bin/modeler.rs` is 2.5k lines doing everything
It mixes MCP tool definitions (48 `#[tool]` fns plus args structs), the Bevy
app, the preview UI (orbit camera, picking, annotation typing, G panel), the
agent camera rig, the screenshot pipeline and the stdio/tokio bridge.

**Fix:** split it into `src/server/{tools.rs, args.rs, bridge.rs}` and
`src/preview/{app.rs, camera.rs, picking.rs, panel.rs, agent_rig.rs,
capture.rs}`, keeping the bin a thin `main`. Most tool fns are
`self.run(move |s| s.foo(args.x))` boilerplate, so a small macro or a generic
`run_with::<Args>` helper would also cut it down. This is pure moves, with no
behavior change. Do it *before* B3/B4 so those diffs stay readable.

### B6. `commands.rs` mixes the session API with lints and validation
`commands.rs` is 1.5k lines. Move `validate`, `lints`, `expect_*`,
`check_rebuild` and `world_signed_volume` into `src/verify.rs`, as functions
over `&Model`, so they're testable without a `Session`.

### B7. Error type is `String` everywhere
`CmdResult = Result<String, String>`. `thiserror` is already a dependency but
looks unused. Add `enum ModelerError { NoPart, Cycle, Csg, Io, Parse, .. }`.
It enables structured MCP errors and lets tests match variants instead of
substrings. Lower priority; do it after B4.

### B8. `mcp_driver.py` is a workaround for a config gap
The STATUS notes that the MCP client was never pointed at the binary. Add a
checked-in `.mcp.json` (project-scoped Claude Code MCP config) that runs
`target/debug/modeler` (or `cargo run --quiet`). Agents then get native tools,
and the driver becomes a test harness rather than the main path.

## C. Smaller cleanups
- `graph::upsert` returns `Result` but can never fail. Make it return `()`.
- `graph::eval_order` is O(n²) per Kahn pass (the inner `find` scans plus a
  full rescan loop). That's fine at current sizes; build a name → producer
  index once if graphs grow past a few hundred ops.
- `Op::RadialArray` calls `uv::ensure_uvs(.., "box")` on every copy, which
  re-projects UVs that the source already had. Copy them and skip the
  re-projection.
- `Model.version` is bumped by the caller in some paths (`new_model` sets it
  by hand). Centralize bumping inside `commit`.
- `session.json` and `--model` restore have no schema version field. Add
  `format_version: u32` now, before the format changes again (B1, B3 will
  change it).
- Root has three findings/MODELS docs. Move them to `docs/` and keep
  STATUS.md current or delete it (it duplicates AGENTS.md).

## Suggested order
A1–A7 (small, independent bugs), then A8 docs, then B5 (split files), then
B4 (enums), then B2 (eval cache), then B3 (graph-level hide/rename), then B1
(history sharing), then B6/B7/B8 and C as time allows.
