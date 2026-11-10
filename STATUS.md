# STATUS.md — where this repo stands

Updated: 2026-11-10 (end of a two-day agent-driven build-out).

## What this is

**modeler** — a standalone mesh-modeling tool built specifically to be driven
by AI agents over MCP, with a live Bevy preview window for humans. ~6.9k
lines of Rust across 9 modules, 46 MCP tools, 16 commits, all pushed to
origin/main.

Architecture (details in AGENTS.md):
- **Op graph** (`src/graph.rs`) — models are ordered op lists evaluated in
  dependency order (Kahn's algorithm with forward-reference healing), not
  raw geometry. Every edit is an upsert that re-derives.
- **History tree** (`src/lib.rs`) — full-model snapshots with cursor;
  undo/redo/jump; editing after a jump branches.
- **Prefab catalog** (`src/catalog.rs`) — non-destructive JSON saves
  (ops + history tree + annotations; parts omitted when fully procedural)
  with atomic writes.
- **MCP server** (`src/bin/modeler/` (`server/` = MCP, `preview/` = Bevy), rmcp 1.8 on stdio) — plus a Bevy
  preview window, an offscreen agent camera rig, and a stdio driver
  (`mcp_driver.py`, with daemon mode) for agents whose MCP tool list is
  frozen.

## Agent-facing capability

Build: `apply_ops` (whole models in one call, graph ops + baked edits with
per-item errors), `op_primitive` (with optional rotation quat),
`op_transform`, `op_boolean`, `op_radial_array` (x/y/z axis),
`op_linear_array`, `op_mirror`, `op_material`, `op_delete`, `op_get`,
`ops`, `new_model`.

Verify (deterministic, no renderer): `validate` (manifoldness + winding),
`expect_parts`, `expect_bounds`, `check_rebuild`, `probe_ray`,
`session_log`. Render: `agent_shot` / `agent_views` (offscreen ortho/
perspective camera rig, auto-framed, independent of the human window),
`screenshot`, `set_camera`.

State: autosave after every mutation + restore on startup (unnamed WIP
survives restarts), `undo`/`redo`/`jump`/`history`, `save_prefab`/
`load_prefab`/`catalog_list`/`catalog_describe`, annotations, `hide_part`/
`show_part`, `export_model` (glb/gltf, skips hidden parts).

Operating guidance rides in the MCP `initialize` response (`instructions`),
so clients get the critical semantics in context without reading AGENTS.md,
which is the long-form reference.

## Content

- 19 prefabs in the catalog (2 pre-existing flowers untouched; 17 built by
  agents across three sessions: 6-model catalog batch, a 3-model session
  that hit the daemon hang, and a 5-model shakedown).
- 6 exported `.glb` files in `models/` (the original catalog batch).
- Three findings documents: `findings-agent-build-session.md`,
  `findings-agent-shakedown.md`, and this file. MODELS.md documents the
  original 6 models op-by-op with history trees.

## Hard-won correctness (all regression-tested)

- Dependency-order evaluation — upsert-position staleness is healed
  automatically; true cycles fail naming the ops.
- Booleans come from csgrs 0.3 (polygon soup) with a bounded O(n) weld
  (1e-4 grid, degenerate drops). Known honest limitation: curved cuts can
  keep small seam edges; `validate` reports them instead of lying, and
  curved-on-curved booleans should be composed/faked instead (documented).
- `disc`/`washer`/`plane` are double-sided; verify tools and lints skip
  hidden parts; primitive ops take rotation quats.
- All render waits time out after 15s — a stuck frame can't wedge the
  server. The historical daemon hang was root-caused (serialization
  re-evaluating boolean snapshots) and fixed; the previously-hanging
  batch shape now completes in ~1.1s.
- Agent camera rig has its own key+fill lighting, matches the human
  window's clear color, and one-shot-per-frame capture semantics.

## Known limitations / open ideas

- Boolean meshes can still carry small seam edges (honest, reported).
- `rename_part` is baked — a graph-level rename op would survive
  re-derivation (queued in findings-agent-shakedown.md).
- No ground grid in the agent rig (spatial judgment aid).
- The workspace MCP config was never pointed at this repo's binary —
  agents here use the driver; wire the MCP server in the client to get
  native `mcp__modeler__*` tools (requires a NEW session).
- Possible next steps: MCP prompts (guided recipes), resources (serve
  AGENTS.md), per-call latency metrics for renders, texture/UV tooling.

## Hygiene

- Tests green; `cargo clippy --all-targets -- -D
  warnings` clean; `cargo fmt` applied.
- `.zcode/` gitignored. `mcp_driver.py` committed (daemon + client modes).
- Everything pushed to origin/main as of this writing.
