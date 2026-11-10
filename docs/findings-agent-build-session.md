# Findings: building 5 models over the MCP driver (2026-11-10)

## What happened

Session stopped early at the user's request after the driver hung mid-batch
(fixing table-leg translations + `expect_bounds` + `agent_shot` + `save_prefab`
never returned; two `TaskOutput` waits timed out, then the batch died when the
daemon was killed). The daemon log showed only "modeler daemon ready" — no
error, no crash. The hang reproduced on the *same kind of batch* that worked
before (apply_ops with upserts of existing names + shots), so it's worth a
look: possibly `agent_shot` inside a batch after upserting ops, or
`save_prefab` after `agent_shot`, blocking the bridge (tokio mpsc/oneshot) or
the render wait.

Completed and saved as prefabs (3 of 5):
1. **mushroom** — sphere cap flattened via transform scale, 5 white dots on a
   radial ring. ~4 tool calls to get right.
2. **snowman** — 3 spheres, cone nose, eyes, stick arms, buttons. 2 calls.
3. **table_chair** — box table with 4 radial-array legs + chair (seat, 4 legs,
   back). Took ~5 calls; hardest so far, all from semantics below.
Models 4 (house with boolean door/window cutouts) and 5 (windmill) were not
attempted.

## Bugs / doc mismatches found

1. **Timeout/hang (unresolved)**: a batch of `apply_ops` (upserts) +
   `hide_part` + `expect_bounds` + `agent_shot` + `save_prefab` hung the
   daemon with no log output. Earlier nearly-identical batches succeeded, so
   it's state- or ordering-dependent. Needs repro and a fix (server-side
   timeouts on render requests would help).

2. **AGENTS.md parameter shapes are wrong/underspecified** — cost 3 failed
   calls at cold start:
   - `size` is **always `[f32;3]`** (sphere ignores [1]/[2], cylinder uses
     [0]=radius [1]=height), but AGENTS.md reads like `size:[0.9]` is legal.
   - `base_color` is **always `[f32;4]` RGBA** (both the standalone tools and
     `apply_ops`). AGENTS.md never says; a 3-component RGB fails deserialization.
   - `hide_part`/`show_part` take `part`, not `name`.
   - `save_prefab` **requires `prompt`** — AGENTS.md lists it as optional.
   - `expect_parts` takes `parts`, not `names`.
   Suggestion: these all cost one opaque "failed to deserialize parameters"
   round-trip each. The MCP tool schemas have the truth; AGENTS.md should
   match.

3. **Ops evaluate alphabetically by name, not batch order** (this is what
   "sequential by name" actually means). A radial op named `leg` referencing
   input op `leg_s` failed with "no part `leg_s` earlier in the graph" even
   though the transform op came first in the same `apply_ops` batch — `leg` <
   `leg_s` sorts first. I had to rename the radial to `legs`. This silently
   bites whenever a copy op's name sorts before its input's name. Doc should
   spell out "alphabetical by output name", and ideally evaluation should be
   batch order with alphabetical only as a fallback.

4. **`op_transform` translation is absolute, not additive.** Template at
   `[0, 0.375, 0]` + transform `translation:[0.45, 0, -0.3]` lands at y=0, not
   y=0.375 — parts sank under the floor. Every copy offset must restate the
   template's full position. Doc says "place... into a new output" which
   suggests compose; it's a replace. Also true of rotation (absolute quat).

5. **Hidden parts leak into the verification tools.** `expect_parts` reports
   hidden template parts as "unexpected", and `expect_bounds` min-y included a
   hidden part (reported -0.375 from the hidden `leg_s` while the doc says
   "over visible parts only"). Either filter hidden parts or document it; as
   is, "expect only what you see" fails while templates are hidden.

6. **`scene_summary` default-material warnings fire on hidden template parts**
   (`nose_u`, `btn_u`, ...) — noise that drowns real warnings.

7. **`apply_ops` primitive ops accept no `rotation`** (standalone
   `op_primitive` doesn't either). Every rotated primitive needs an extra
   primitive + `op_transform` pair — that's why the snowman is 25 ops instead
   of ~14. A rotation field on Primitive would halve many graphs.

## Things that worked well

- `apply_ops` batching is genuinely fast: the snowman went up in one call, and
  the per-op ok/error log with rollback made failures easy to localize.
- `scene_summary` warnings caught every real mistake (radial source overlap,
  z-fighting, uncolored copies) before I looked at any render.
- `agent_shot` + `--save-shot` gave a real check: it exposed the hidden-but-
  rendered white cap immediately (which is how I discovered the hide calls had
  failed on the wrong arg name).
- Upsert-and-rerderive (fixing `leg_s` translation in one op) repaired a
  broken model without rebuilding.
- Autosave meant killing the daemon lost nothing.
- Prefab saves worked first try once `prompt` was supplied: mushroom.json,
  snowman.json, table-chair.json all in the catalog.

## The gotchas I saved to memory (these need to be obvious)

These are the rules an agent has to know *before* the first `apply_ops` —
every one of them cost a failed call or a silent rebuild this session. They
belong in AGENTS.md (and ideally the MCP `initialize` instructions), not just
in my private memory:

1. **Ops evaluate ALPHABETICALLY by output name, not batch order.** A radial
   op named `leg` referencing input `leg_s` failed "no part earlier in graph"
   because `leg` < `leg_s` sorts first. Name copy-ops so they sort after
   their inputs (e.g. `legs` after `leg_s`).
2. **`op_transform` translation/rotation are ABSOLUTE, not additive.** A
   template at y=0.375 + transform translation y=0 sinks to y=0. Restate the
   full position in every copy.
3. **Wire shapes are strict and undocumented:** `size` is always `[f32;3]`
   (sphere radius = size[0]; cylinder r = size[0], h = size[1]);
   `base_color` is always RGBA `[f32;4]`, even in `apply_ops`. Wrong shapes
   only yield "failed to deserialize parameters: invalid length N" with no
   field name.
4. **Argument names are strict too:** `hide_part`/`show_part` take `part`;
   `expect_parts` takes `parts`; `save_prefab` REQUIRES `prompt` (it's
   presented as optional).
5. **`apply_ops` primitive ops have NO rotation** — rotated primitives need a
   primitive + `op_transform` pair (the snowman needed 25 ops instead of ~14).
6. **`expect_parts`/`expect_bounds` include HIDDEN parts** despite docs
   saying "over visible parts only" — hidden templates show up as
   "unexpected" and stretch the bounds. Default-material warnings also fire
   on hidden templates.
7. **Material ops must run BEFORE any array/mirror/transform that copies the
   part** (copies clone the source at eval time — uncolored copies otherwise).
8. **Booleans keep every intermediate as a scene part** that z-fights with
   the final shell; reuse cutters in place and recolor intermediates, since
   `op_delete` can't remove them while later ops derive from them.
9. **Radial array full ring:** count-1 copies with `start_degrees = 360/count`,
   else the source and copy 0 coincide. Also note the copy-naming asymmetry:
   radial outputs `name_0..n-1` (copy 0 lands at start_degrees); LINEAR
   outputs `name_1..n-1` (source is the unnamed copy 0).
10. **Tent/roof from one wedge:** scale z by 0.5–0.65, shift so the wedge's
    vertical face sits at the ridge, then `op_mirror` across z for the other
    half.
11. **`jump(id)` restores that snapshot's op list exactly** — everything after
    the jump node must be re-issued in the new branch. Editing at the tip
    never branches; `history` calls create no history node.
12. **History ids are positional and start at #0** (`set_model_name` is node #0).

Items 1–6 are from *this* session; 7–12 carried over from the earlier
6-model catalog session and are already reflected in AGENTS.md — but 1–6 are
not, and 1–2 in particular look like renderer bugs ("why is my leg under the
floor?") rather than semantics.

## Suggestions (priority order)

1. Fix the hang; add a timeout to the render/shot bridge so a stuck frame
   can't wedge the whole server.
2. Make verification tools (`expect_parts`, `expect_bounds`) skip hidden
   parts; suppress material warnings for hidden parts.
3. Evaluate `apply_ops` in batch order (fall back to name order across
   separate calls), or at minimum detect name-cycle errors and say
   "op `leg` sorts before its input `leg_s`".
4. Add `rotation` to primitive ops.
5. Sync AGENTS.md with the real schemas and the gotcha list above — items 1–6
   especially: alphabetical op evaluation, absolute transform translation,
   exact wire shapes/arg names, hidden-part leakage into verification tools.
   These read as bugs from the outside and cost a rebuild each; a paragraph
   each in AGENTS.md would have saved most of the failed calls this session.
