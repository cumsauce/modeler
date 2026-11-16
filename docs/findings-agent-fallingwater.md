# Fallingwater session findings (2026-11-16)

Built a full Fallingwater model (exterior + photograph-based interiors of
every floor) in one session, `fallingwater-house`, final state v63301:
324 parts / 760 ops / 5 point lights, catalog-saved. Session ran alongside
**another agent that was churning through a new document every 1–3 minutes**
on the same daemon, which turned this into a stress test of multi-agent
behaviour. Everything below was observed, not inferred.

## What held up

- **Atomic batches + upserts are the recovery mechanism.** Three times my
  in-progress work was lost to document swaps; three times re-issuing the
  identical `apply_ops` batch restored it exactly (upserts are idempotent).
  A single 203-item batch (full interiors) applied fine — batch size is not
  a limit.
- **`verify` caught two real design mistakes** I could not see in renders:
  (1) furniture placed "inside" the service wing was actually inside a
  *solid* stone block — invisible and flagged as collisions; (2) the
  interior stair ran into the chimney's footprint. Both were
  won't-see-it-in-a-render class bugs. `grounded`/`collisions` with a
  furniture allow-list is now part of my standard loop for interiors.
- **`shell`** fixed the solid-building problem cleanly (`open: "back"`
  hollowed the service wing toward the living room, furniture then reads
  inside a room).
- Renders stayed deterministic and comparable across iterations; the
  `agent_views` grid carried the whole visual QA load.

## Issue 1 — cross-agent document races (critical)

Observed, in order of severity:

1. **Mutations landing on another agent's active document.** Three times a
   batch applied into `muse` / `muse_final` / `nyc-kit-brownstone` while I
   was working on my doc. Each result header *did* name the wrong model,
   and a single `undo` on that doc removed my whole atomic node — clean,
   but it costs a cycle and one time it intermixed with their concurrent
   edit (my undo then reverted their tweak's parent state).
2. **Catalog namespace clobbering.** The other agent renamed their doc to
   `fallingwater` (a name my doc held) and `save_prefab`ed over that slot;
   minutes later my open document was closed. `catalog_describe` then
   showed *their figure* under my house's name.
3. **Close-storms.** With the other agent churning, my open document was
   closed between calls repeatedly; once the daemon re-opened it *from the
   stale catalog copy* mid-session, silently dropping two batches from the
   live doc (part count in the result exposed it: 252 → 241 parts).
4. The daemon's auto-switch-back ("switched back to your X … quietly from
   now on") and the per-call result header are the only defenses, and both
   worked — but prevention is on the agent, not the tool.

**Working playbook** (should be promoted into AGENTS.md):

- `[scene switch, apply_ops]` back-to-back in one message; read the
  `model \`X\`` line of every mutation result; on mismatch, `undo` there
  and re-apply.
- `save_prefab(force: true)` immediately after finishing any milestone —
  the catalog is the only durable thing that survives close-storms and
  restarts. Save under a name nobody else will plausibly use.
- If the doc gets swapped for a stale copy, re-issuing the lost batches is
  safe (upserts), and cheaper than debugging state.

**Recovery recipe** (after a close, when work postdates the last catalog
save) — the *supported* paths all failed:

- Editing `open_docs.json` + restart does **not** work: the dying daemon
  flushes its own (empty) list over your hand-edit on shutdown, and
  `close_saved_documents` drops any doc whose name matches a catalog file.
- `parked_docs.json` hand-editing races the live daemon's `save_parked`.

What worked: copy `prefabs/session.history.<doc>.jsonl` (append-only,
survives everything — 191 MB in my case) to a **new** doc name, then
hand-write a catalog entry: `prefabs/<new>.json` (last journal line's
`model`, renamed, history stripped) + `prefabs/<new>.history.json`
(`{"nodes":[…journal lines…]}`) + append to `prefabs/catalog.json`
`entries`. `scene open <new>` reads it from disk, no restart involved.
Recovering 37 history nodes this way was lossless.

## Issue 2 — MCP 30 s client timeout vs server work (major)

- Heavy `apply_ops` (~75–200 items) took 60–90 s server-side; `verify`
  with `collisions` over ~200 parts took 47–53 s. The MCP tool times out
  at 30 s **while the server keeps working**. Two consequences:
  - an "error: timed out" that actually applied (retry blindly and you
    double-apply — harmless for upserts, wrong for `repeat`-grown ops or
    if the batch wasn't idempotent);
  - every timeout widens the cross-agent race window.
- Workaround: `python3 mcp_driver.py --connect --calls <file> --json` from
  Bash with a long timeout for anything heavy. Worth either raising the
  MCP timeout for `verify`/big batches or returning a job id.

## Issue 3 — pattern materials don't see later batches (gotcha)

A `material` item with a pattern (`"stone_*"`) colours parts existing when
that op *runs in its own batch*; parts with the same prefix added in a
**later** batch are not coloured (the upserted op keeps its original
position). Cost me a patch batch for 9 `stone_*` parts and again for 8
`tree*_leaves`. Either document this loudly, or re-evaluate pattern ops
against every batch's new parts.

Related: shrinking a `linear_array` (8→6) failed because earlier material
ops referenced the now-vanished copies ("no part `tbeams_s_6` earlier in
the graph") — grow-and-shift worked instead; shrinking should probably
cascade-delete dependent per-copy ops.

## Issue 4 — smaller gotchas (all worked around)

- Spheres must be `{"type":"primitive","kind":"sphere"}`; a bare
  `{"type":"sphere"}` gives "unknown type", and a half-fixed resend gives
  "missing type" — the error could name the closest valid spelling.
- `shell.open` takes world-axis words (`front` = +z); compass words
  (`south`) are rejected. A compass alias would match the rest of the
  tool's orientation vocabulary.
- `scatter.near`/`near_distance` is **3D** distance to the near part's
  surface: a 3.4 m tall basin wall needs `near_distance` ≈ 4.5, not 3.
  Worth a line in the tool description.
- Scattering onto boolean-carved vertical walls needs `faces: "all"` +
  high `max_slope` (~85°), and regions that are mostly carve-void silently
  yield 0 placements (the "placed 0 of N" note does say so — good).
- The remap node's `from`/`to` are `[lo, hi]` arrays; dir_blur repeats via
  `length`; output uses `metallic`. Each cost one retry — `describe_op`
  solved all three, but the first-error hint could quote the schema like
  it does for unknown fields.
- `group` inputs stay visible; hide them explicitly (documented, but easy
  to forget — the double-render is subtle).
- The `boxy` warning fires hard on deliberately rectilinear architecture
  (183/253 parts). An escape hatch (opt-out per part, or a style token)
  would keep it useful for furniture while silencing it for modernist
  buildings.

## Verify ergonomics for interiors

Interior work generates many *intentional* overlaps (mattress/frame,
pillow/cushion, headboard/frame, rug/floor slab, chair under table).
Working set: one furniture-cluster allow list
(`["int_*","int_*"], ["din_*","din_*"], ["bed_*","bed_*"], …` ~20 pairs)
while keeping structural parts (`lvl*`, `stone_*`, `glass_*`, `par*`,
`chimney`) checked against everything. `grounded`'s "held up only by
passing deep into another part" notes are exactly right for spotting
collision-held furniture, and non-strict pass is the right default.

A `verify` preset (e.g. `furniture: true`) that applies sensible
interior allows automatically would remove ~20 lines of boilerplate per
check.
