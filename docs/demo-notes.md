# Demo run: windmill scene (2026-11-10)

Built through the MCP driver on the `review-pass-fixes` build: ground disc,
cylinder tower with a door and window cut by booleans, cone roof, hub, 4 blades
(`radial_array` count 4 about z with `include_source`), a 9-post fence (`linear_array`),
a mirrored back rail, and a two-sphere tree. Hide/rename went in as graph ops.
Calls: `docs/windmill-demo-calls.json`. Shots: `shots/windmill-demo-*.png`.

## What worked
- One `apply_ops` call built 31 ops / 26 parts; a bad batch would have stopped
  with per-item context. Typed enums, dependency order, mirror, arrays all fine.
- `check_rebuild`: cached parts match a fresh (uncached) derivation.
- Hide + rename recorded as graph ops (`#26 visibility ... hidden`,
  `#30 rename tower_cut -> tower_final`) and survive tweaks.
- Re-issuing the tower primitive (re-runs both booleans) or the leaves
  material round-trips in ~0.07-0.09 s including driver overhead.

## Findings from the first run, and what was done (same day)
1. **Boolean pockets lost their inner walls -> FIXED.** Root cause: csgrs drops
   faces when a face of the second operand lies in the same plane as a face of
   the first without overlapping it (two equal-depth cutters). `csg::boolean`
   now nudges the operand ~2 mm off shared planes; regression test
   `equal_depth_cutters_keep_both_pockets`. Probe through the door now hits
   the pocket end wall at z = 0.452.
2. **No lint for bad booleans -> FIXED.** `scene_summary` warns on visible
   boolean outputs with open edges (follows renames).
3. **`session_log` only saw renders -> FIXED.** `call_tool` logs every tool.
4. **Driver noise -> FIXED.** base64 is opt-in (`--b64`); `--save-dir` saves
   every image numbered; `--save-shot` saves extras as `_2`, `_3`.
5. **`probe_ray` first hit only -> FIXED.** `all_hits: true`.
6. **Framing -> FIXED (partly).** `focus: [parts]` on `agent_shot` /
   `agent_views`. The grey backdrop is still open (see below).
7. **Radial array bookkeeping -> FIXED.** `include_source: true`.
8. **`expect_bounds` -> FIXED.** Failures name the part that sets the extreme.

## Still open (found while verifying)
- The large flat grey backdrop in the iso shot is unexplained (not yet checked
  whether it is the ground disc, the ground plane or the camera's clear colour).
- Smooth per-vertex normals across boolean cut edges give dark streaks on the
  tower; recompute normals per face (or by angle threshold) on boolean outputs.
- The boolean lint cannot tell harmless curved-cut seams from real holes
  (108 open edges on `tower_final` renders fine); a ray-based check would.
- The nudge is a workaround for csgrs; a robust boolean backend would remove it.
