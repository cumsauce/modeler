# Shakedown: 5 models of increasing complexity (2026-11-10)

Built with the current toolset end-to-end (apply_ops → expect_parts /
expect_bounds → validate → agent_views → save_prefab), tracking calls,
failures, warnings, and recoveries. Runs against the daemon; every claim
below was observed, not inferred.

## The models

| # | Model | Ops | Tool calls | Failed calls | Visual iterations | New ground covered |
|---|---|---|---|---|---|---|
| 1 | mug | 6 | 21 | 3 | 4 | torus primitive, curved-curved booleans, disc orientation |
| 2 | lamp-post | 10 | 14 | 1 | 1 | primitive rotation in real use, catalog-overwrite trap |
| 3 | windmill | 17 | 5 | 0 | 0 | horizontal-axis blades (manual), boolean door on curved tower |
| 4 | cottage-garden | 30 | 7 | 1 | 0 | linear_array fence, cutter-reuse pattern at scale |
| 5 | arch-bridge | 23 | 15 | 1 | 1 | boolean arch cut, two linear arrays, hide_part in workflow |

(Tool calls = JSON-RPC calls, not invocations. "Failed calls" = errors that
cost a retry.)

All five are in the catalog: mug, lamp-post, windmill, cottage-garden,
arch-bridge.

## What worked (and is worth keeping exactly as is)

1. **apply_ops is the difference between one call and twenty.** Cottage
   (30 ops) and bridge (23 ops) went up in single calls with per-op
   ok/error logs. The snowman finding from the last session held.
2. **The warnings section is now trustworthy.** After the hidden-part
   fixes, zero false alarms this session, and it caught every real
   mistake before any render: uncolored copies, radial source overlap,
   material-after-copy ordering.
3. **validate caught two genuinely broken meshes** (the corrupted mug
   boolean, 242/4234 bad edges) that scene_summary called perfectly
   fine. This is the tool doing exactly its job.
4. **expect_bounds found the invisible geometry bug**: the mug's coffee
   "disc" reported max y 1.24 against an expected 0.9, which is how the
   vertical-disc trap was discovered. A numeric assertion did what two
   screenshots had normalized away.
5. **agent_views auto-framing** needs zero camera work — front/top/iso
   all landed correctly on every model, including a 4-unit bridge.
6. **Autosave + new_model** made model switches free; nothing was lost
   across ~15 daemon interactions.

## What didn't work

### Tool gaps (new this session)

1. **`hide_part` is not a valid op type inside `apply_ops`** — and the
   batch deserializer is all-or-nothing, so one unknown variant rejected
   a 23-op build with "unknown variant `hide_part`". Cost a full retry.
   Fix options: accept baked-edit specs in batches (apply after ops), or
   per-item deserialization errors.
2. **`save_prefab` overwrote an existing catalog entry silently.**
   Building the lamp without a leading `new_model` left the model name
   "mug", and saving replaced the mug prefab with a mug+lamp chimera.
   expect_parts caught it ("unexpected: body, coffee, handle") but the
   save had already happened. Fixes: save_prefab echo should say
   "overwrote existing entry"; scene_summary could lead with the model
   name; a `--new` guard on save would help.
3. **Open primitives are single-sided.** A disc rotated flat is invisible
   from above (back-face culled) with zero errors — two silent
   iterations of "where did my coffee go". Fixes: generate
   disc/washer/plane double-sided (also removes their permanent
   non-manifold flags), or at least warn in the ops listing.
4. **Curved-curved booleans produce non-manifold garbage.** The first
   mug (cylinder−cylinder cavity + torus union) rendered as corrupted
   noise; validate reported 242/4234 bad edges. Box-cylinder and
   box-box booleans are fine (the windmill door and bridge arch render
   cleanly with only small seams). Document "booleans on curved
   surfaces: compose or fake instead" — or clean up csgrs output
   (weld/dedupe/degenerate-triangle removal).
5. **`validate` on an empty model said "all parts: manifold,
   outward-wound".** Fixed already (now "no parts to validate").
6. **The agent camera's background was near-black**, mismatching the
   human window and making silhouettes hard to read. Fixed already
   (agent camera now uses the world ClearColor like the main window).
7. **Low-roughness parts under the agent camera's directional light**
   produce specular blobs that read as solid geometry (the mug's
   roughness-0.1 "coffee" looked like a white dome). Consider a softer
   key light for the agent rig, or document "roughness < 0.15 will show
   a hot highlight in agent views".

### Known-and-confirmed (from findings-agent-build-session.md)

- **Material-before-copies still bites** (stool legs were white on the
  first attempt): material-vs-copy has no dependency edge, so list
  position still decides. Dependency-order eval fixed positions, not
  this. A deeper fix would add material→copy edges to eval_order.
- **Radial array is +Y only**: windmill blades needed 8 ops (4 rotated
  primitives + 4 materials) where an `axis` parameter would need 2.
- **In-place cutter reuse keeps confusing names**: the cottage's glass
  is literally named `win_l_cut`, so expect_parts lists look wrong.
  `rename_part` is a baked edit that refresh overwrites; an op-level
  rename would fix the ergonomics.
- **validate flags booleans systematically** (34–160 seam edges even on
  clean-looking results). Honest but noisy; see #4's cleanup idea.

## Metrics that paid for themselves this session

- **expect_bounds caught invisible geometry** (the vertical disc) that
  two screenshots normalized. Numeric assertions beat renders for
  "is it where I think it is".
- **expect_parts caught the catalog overwrite** ("unexpected: body,
  coffee, handle") before I wasted iterations on the chimera.
- **validate caught the only truly broken mesh.** Manifoldness is a
  reliable boolean-vs-composed quality signal: composed models pass,
  boolean outputs carry seam edges.
- **The warnings section had zero false positives** post-fix — when it
  speaks, something is actually wrong. This is the single highest-value
  signal per token.
- **Call counts as effort metric**: composed-with-batches models (5–7
  calls) vs boolean-heavy or trap-hit models (14–21). If call count
  spikes, there's usually a tool gap, not agent incompetence — every
  spike this session mapped to a concrete gap above.

## Improvement queue (priority order)

UPDATE 2026-11-10, later the same day: items 1-5 are done (batchable baked
edits with per-item errors, double-sided open primitives, boolean weld-only
cleanup replacing the hang-prone repair pipeline, radial `axis` param,
save_prefab overwrite warning), plus: dependency-order op evaluation, an
agent offscreen camera rig (`agent_shot`/`agent_views`), 15s timeouts on all
render waits, a `session_log` tool, flag-based `parts_derive_from_ops`
(serialization no longer re-evaluates snapshots — the daemon hang's root
cause: the same boolean-heavy batch that hung now completes in ~1s), and the
agent camera background/lighting fixes. Remaining from the queue: op-level
rename (batch rename is baked and refresh-visible names revert — wait, no:
baked renames persist but ops re-derive the original name alongside), ground
grid, and the session-log could grow per-call timings for renders only.

1. Batch-able baked edits (hide/show/material/rename in apply_ops) or
   per-item deserialization errors.
2. Double-sided open primitives (disc/washer/plane).
3. Boolean output cleanup (weld verts, drop degenerate triangles) —
   would simultaneously fix the mug corruption and silence most
   validate seams.
4. `axis` parameter on radial_array (Y / X / Z).
5. save_prefab overwrite warning + model-name lead in scene_summary.
6. op-level rename so reused cutters can carry semantic names.
7. Optional ground grid for the agent rig (spatial judgment on height/
   alignment); softer agent light or roughness guidance.
8. A session-log tool (per-call ok/latency) so agents can self-audit a
   build; per-call timing in results would also give latency metrics for
   free.
