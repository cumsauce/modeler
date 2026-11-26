# Level scenes (.bsn)

**Level scenes (`.bsn`):** a map document (terrain, roads, `prefab`
instances, `marker`s) exports as a scene with `export_model("maps/commune.bsn")`
(`src/scene.rs`), in jackdaw's BSN format (Bevy 0.20 syntax), which
`jackdaw_runtime`'s `JackdawSceneRoot` spawns:

- every `prefab` item is an entity with a `GltfSource` pointing at
  `models/prefabs/<prefab>.gltf`, exported once (`optimize: "game"`) and
  again only when the prefab file is newer: instances by reference, not
  baked copies;
- every `marker` is an entity at its spot (yaw as rotation);
- the document's own visible parts (ground, roads) become one glTF beside
  the scene and one `<stem>_ground` entity;
- each `terrain` writes `<stem>.<terrain>.heights` (magic `MHT1`; u32 nx, nz;
  f32 x0, z0, cell, h_min, h_max; nx*nz u16 heights, row-major by z; nx*nz
  u8 mask, bit 0 path, bit 1 pad), so the game reads ground height without
  the scene (`scene::read_heights`).
- Entities are sorted by name with stable `SceneNodeId`s (a hash of scene
  and name), so re-exports diff cleanly.
- **Components**: `prefab` and `marker` items take `components:
  {"Place": {"name": "wendys"}}`, checked when applied against the game's
  vocabulary `.modeler/components.json`: `{"header": ["// ..."],
  "components": {"Place": {"type": "my_game::places::Place", "fields":
  {"name": "string"}, "doc": "..."}}}` (field types string, f32, i32, u32,
  bool, vec2, vec3, `strings` (a list), and `ref` / `refs`: names of other
  entities in the same map (markers, prefab instances), `String` /
  `Vec<String>` in the game, and the export fails naming any that don't
  exist; omitted fields keep the component default; a component with no
  `fields` is a marker component, written as `{}`). `header` lines open
  every scene file (a project's required comment).
- A map may hide its `terrain` so the export leaves the ground to the
  game (built from the heights file); the window and grid still draw a
  hidden terrain, so the map isn't only marker gizmos over empty space.
  Agent views, verify and exports leave it out as usual.
- **Plain data**: `<stem>.map.json` beside the scene holds the same
  entities (`scene::MapFile`): name, marker `kind` and `size`,
  `translation`, `rotation` (quaternion), `yaw` (degrees, 0 = +z),
  `scale`, `gltf`, `components` by vocabulary name as written, plus the
  terrains' heights files. Read it (serde_json) when code needs the layout
  before the game world exists, instead of parsing the BSN text. A
  marker's yaw is in both files' rotation, its size only in the JSON.
