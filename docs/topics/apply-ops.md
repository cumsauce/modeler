# apply_ops semantics

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

- **Frames** build in another part's own coordinates: a primitive with
  `frame: "paper"` (or `"paper:top-front"`, an anchor on that part's local
  box, same words as pivots) takes `translation` / `rotation` relative to
  that part's transform, origin at the anchor (default its own origin). It
  is a real dependency: tilt the paper (a param, a re-issue) and the framed
  parts follow. A post-graph `move` of the frame part does not carry them:
  list them in the move too. Pair with `linear_array` `local: true`, which
  steps `offset` along the source part's rotated axes (typed lines stepping
  down a tilted page).

- **Params**: `params: {"tower_h": 2}` sets named numbers before the items
  run; any value may be `"$tower_h"` or `"=$tower_h / 2 + 0.1"`.
  Expressions also take comparisons `< <= > >= == !=` (1 or 0), `&&`, `||`
  and `!`; comparisons bind looser than `+ -`: `"=($n >= 2) * 3 - 1.5"`. The model
  remembers which ops read which params, so a later
  `apply_ops({"params": {"tower_h": 3}})` re-derives all of them in one
  history step.

- **`ops_file`**: a JSON array of items in a project file, applied
  *before* the inline `ops`: the file is the base, inline items are tweaks,
  so an inline item with the same type + name wins. Every op a call gives
  more than once (same type + name, `material` items aside: they stack) is
  a warning naming the op and the source (inline / ops_file) that won.
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

- **Re-applying a batch is safe for materials**: an in-place `material`
  identical to the newest one on that part is skipped ("unchanged"), so
  re-issuing a whole batch no longer doubles its material ops.
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
  `tri_mesh`) they edit the part directly. A `uvs` projection on an
  op-derived part is remembered (`Model.uv_projections`) and re-applied after
  every re-derivation (material edits, later batches), so label plates stay
  upright.
- **Op-driven snapshots store no parts**; undo/redo/jump re-derive via the
  eval cache. Snapshots are history-less (the live tree is the single source
  of truth).
- **Jump restores the snapshot's op list exactly**; use `diff` to see what
  the other branch had and `cherry_pick` to bring it over.
- **Save format is versioned** (`Model::format_version`, `FORMAT_VERSION`):
  old files read as 0 and load; files from a newer build are rejected.
  Fully op-derived prefabs save ops, params, templates and annotations
  without parts; the **history goes to a sidecar** `<name>.history.json`
  beside the file (git-ignored in a project, per machine like the
  autosaves), so the tracked file holds only what rebuilds the model.
  Loading reads the sidecar; files saved before it (history inline) still
  load, and `modeler --project <dir> --split-history` moves their history
  out. Writes are `.tmp` + rename.

`scene_summary` warnings: full-ring radial arrays whose copy 0 overlaps the
source (only with `include_source: false`), visible parts still wearing the
default material, parts with identical world bounds (z-fighting; when one
is an input of the op making the other, the warning names it and says to
hide it), boolean
volume sanity (a difference that changed neither its input's volume nor
its surface area = the cutter misses; a thin groove or inlay that cut adds
the walls of its cut, so it stays quiet; a union whose volume exactly sums its inputs = no overlap, unless the union is a cutter fed to a difference) and
boolean outputs with open edges enclosing real area (a safety net; Manifold
outputs are closed), and **floating parts** (off the ground and touching no
other part, e.g. a copy transformed instead of its neighbour). **Quality
lints** (`src/quality.rs`, at most four lines per kind): near misses (a
1-5 mm gap where two parts nearly touch, faces 1-5 mm off flush), a model
mirror-symmetric about x = 0 or its centre plane except for a few parts a
little off (named), runs of numbered or same-size parts with nearly but not
quite even spacing, pairs of parts under 2% apart in size, and **boxy**
(at least 5 and over 70% of visible parts are unrounded axis-aligned boxes:
round, loft, deform taper, smooth subdiv); a style number (or param) `lint_boxy: 0` silences it for a deliberately blocky model; and a
**ragged crease** (a part over 2000 triangles whose sharp joins, edges
over 40 degrees chained into runs of 8 or more, zigzag instead of running
straight: fix as the line says). Volume checks
and welds scale with the part, so centimetre-scale models validate
correctly. Hidden parts are excluded. An empty list means none
fired.
