# Scenes (open documents)

**Scenes (open documents):** `scene(action, name?)` keeps several models
open at once, each with its own ops, history and style; every other tool
acts on the active one. `reset` (clear the active document's ops and parts in place: a rebuild
starts from nothing, undo restores it; closing the only open document
leaves a fresh empty one), `list` (active marked `*`, which prefabs each
instances), `open` (an open doc, else the catalog entry, else a new empty
doc), `switch` (open docs only), `close`. Build a map from pieces: model
`console` in its own doc, `open` `bridge_map`, and place
`{"type": "prefab", "name": "c1", "prefab": "console"}` items. A prefab
item resolves to the **open doc of that name first** (else the catalog
file), and switching back to a scene re-syncs every instance from its
source (new parts, edits, style), so pieces stay live while you iterate.
A prefab item's `style`: omitted keeps the piece's own style; `"scene"`
re-derives it with the scene's tokens layered over its own (scene wins;
re-themes with `style set` on the scene); an object patches tokens for that
instance (`{"colors": {"primary": "#c33"}}`, a red-alert variant). Only
items that read tokens (`@color`, presets) follow. The inactive docs
autosave to `open_docs.json`. On `--restore` **no document reopens by
itself**: one that matches its catalog file is dropped
(`Session::close_saved_documents`; it opens from the catalog on demand) and
every unsaved one is **parked** (`Session::park_restored`, `src/parked.rs`)
while the server starts on an empty document, so a restart never puts a
stale copy in front of agents. A parked document is its own file,
`parked/<project>-<hash>/<name>.jsonl` next to the prefabs (`library/` for
the shared library; `parked.<slot>/` for a slot server): its first line is
the index entry (name, project, op count, version, last editor, when it was
parked), its second the model. Startup reads only the first lines; the body
is read, and its parts derived, only by `scene restore` NAME or a `doc` that
names it, and parking or dropping one writes or removes only its own file.
An older single `parked_docs.json` is split into these files on first start
and kept whole in `archive/` (one that does not parse is moved aside to
`parked_docs.bad.<time>.json`). `scene list` / `scene restore` show the parked ones,
`scene restore` NAME opens one (refused while a document of that name is
open: close it first), `scene close` NAME drops it. Humans find them in the
document popup (⌘K) under "Recovered after restart"; a click restores one
(closing a saved copy of the same name first). The window's outliner
lists the open documents (click to switch).

Saved documents nobody has touched for 20 minutes are **closed
automatically** (`IDLE_CLOSE`): only when unchanged since they were saved or
opened from the catalog (restored ones are compared with their file), so no
work is lost; opening one again loads it from the catalog (a tool call
whose `doc` names it does so automatically). Unsaved documents stay open.
