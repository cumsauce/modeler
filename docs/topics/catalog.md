# Model, catalog, output

**Model, catalog, output:** `new_model(name)` (starts beside a non-empty
document, never replacing it; an open name is refused), `set_model_name(name)` (the prefab entry name), `save_prefab(description,
tags, prompt, force?)` (reports when it overwrote an entry; refuses, unless
`force`, when the catalog file changed since this session loaded or saved
it: another agent or server saved it in between),
`load_prefab(name)` (answers with a brief: counts, visible parts, params,
named moves, warnings), `catalog_list` and `catalog_describe` (`delete: true` removes an entry;
its file moves to `prefabs/removed/`; each entry also
lists its knobs: params with values and named moves/places, read from the
file),
`export_model(path, "gltf" (default) | "glb")` (skips hidden parts).
