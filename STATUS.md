# STATUS.md — where this repo stands

Updated: 2026-11-11 (Manifold booleans, API trap removal, 28 tools).

## What this is

**modeler** — a standalone mesh-modeling tool built specifically to be driven
by AI agents over MCP, with a live Bevy preview window for humans. ~6.9k
lines of Rust across 11 modules and 28 MCP tools, on branch
`review-pass-fixes`.

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

Build: `apply_ops` is the one build tool: typed items (primitive,
transform, boolean, arrays, mirror, material, prefab, hide/show, rename,
delete, tri_mesh, uvs), lenient sizes/colours, errors that name the item
and field, named `params` with `$refs`, and an `expect` block. Plus `ops`,
`op_get`, `describe_op` (example + schema per item type), `new_model`.

Verify: `verify` (parts, bounds, validate, rebuild), `probe_ray`,
`session_log`. Render: `agent_views`, `screenshot`, `set_camera`.

State: autosave + restore, `undo`/`redo`/`jump`/`history`/`history_prune`,
`diff`/`cherry_pick`, prefab catalog, annotations, `export_model`.
`scene_summary`, `apply_ops` and `verify` also return structured JSON.

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
- Booleans run on Manifold (`manifold-csg`): closed outputs, exact coplanar
  and curved-on-curved cuts, input UVs preserved.
- `disc`/`washer`/`plane` are double-sided; verify tools and lints skip
  hidden parts; primitive ops take rotation quats.
- All render waits time out after 15s — a stuck frame can't wedge the
  server. The historical daemon hang was root-caused (serialization
  re-evaluating boolean snapshots) and fixed; the previously-hanging
  batch shape now completes in ~1.1s.
- Agent camera rig has its own key+fill lighting, matches the human
  window's clear color, and one-shot-per-frame capture semantics.

## Known limitations / open ideas

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
- Work is on `review-pass-fixes`; no PR opened yet.
