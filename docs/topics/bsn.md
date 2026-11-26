# Level scenes (.bsn, .tscn)

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
- every static `mesh` collider of the map's own parts (a `carve` with
  `collider: "mesh"`, a `collider` rule with shape `mesh`) goes into one
  `<stem>.collision` file (magic `MCL1`; u32 vertex and index counts; f32
  x, y, z per vertex in the map's frame; u32 indices, three per triangle;
  little-endian), named in `map.json` as `collision`, so the game builds an
  exact collider for sloped and curved floors without loading the glTF
  (`scene::read_collision`). No mesh colliders: no file (a stale one is
  removed).
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

## Godot scenes (.tscn)

`export_model("maps/commune.tscn")` (or `format: "godot"`) writes the same
map as a Godot 4 text scene (`src/godot.rs`), with the same glTFs, heights,
`.collision` and `map.json` beside it:

- `res://` paths start at the nearest folder above the scene holding
  `project.godot`, else at the asset folder; export inside the Godot
  project. A `project.godot` makes its folder a modeler project (the
  catalog in `.modeler/`), and relative exports land in its `assets/`,
  or at its root when it has none (`"maps/level1.tscn"` ->
  `res://maps/level1.tscn`, shared textures in `res://textures/`). Prefab instances and the `<stem>_ground` geometry are
  instanced glTF `PackedScene`s (Godot imports the glTF itself); markers
  are `Marker3D` with `metadata/kind` and `metadata/size`; the root has
  `metadata/map_json`. Names Godot rejects (`. : @ / " %`) become `_` and
  keep the original in `metadata/modeler_name`.
- **Components** go into `metadata/components`, a dictionary by
  vocabulary name (`{"Place": {"name": "depot", "radius": 4.0}}`): vec2 /
  vec3 as `Vector2` / `Vector3`, strings / refs as `PackedStringArray`.
  The vocabulary's `type` (the Rust path) is optional for a Godot-only
  project; a `.bsn` export of a component without one fails.
  `metadata/variant` names a prefab instance's variant (apply it in game
  code).
- **Scripted components**: a vocabulary entry with `"godot": {"script":
  "res://components/loot.gd", "node": "Node"}` (`node`: the class the
  script extends, default `Node`) also becomes a child node of the entity
  named after the component, running that script with each field set as a
  property (`label = "gold"`, `weight = 2.5`, `at = Vector3(1, 2, 3)`,
  `tags = PackedStringArray("a", "b")`): declare them `@export var` with
  those types (`ref` / `refs` are node names: `String` /
  `PackedStringArray`). A field named like a Node property (`name`,
  `owner`, `position`, `visible`, ...) fails the export: rename it.
- **Terrain** becomes a `<terrain>_heightmap` `StaticBody3D` with a
  `HeightMapShape3D` (exact heights, cells scaled uniformly), replacing
  any mesh collider on the terrain part, whether the terrain is shown or
  hidden. Non-square cells get none (reported).
- **Colliders** become physics nodes (Godot ignores `extras.collider`):
  per entity a `collision` `StaticBody3D` (`collision_kinematic`
  `AnimatableBody3D`, `sensor` `Area3D`) with one `CollisionShape3D` per
  collider (box `BoxShape3D`, hull `ConvexPolygonShape3D`, all mesh
  colliders one `ConcavePolygonShape3D` named `mesh`). A prefab's shapes
  are shared sub-resources and its bodies children of every instance; the
  map's own are `<stem>_collision` / `<stem>_sensor` at the root. Dynamic
  bodies are left out and reported (a `RigidBody3D` must own the mesh it
  moves).
- Textures are shared PNGs (`textures: "png"`; `embed` becomes
  `embed_png`): Godot doesn't read KTX2 from a glTF `source` and
  re-compresses on import. A prefab glTF still holding KTX2 from a `.bsn`
  export is re-exported. `lods` is refused (Godot would draw every level;
  it makes its own LODs when it imports), and so is `ao` (Godot 4.7's
  importer ignores `texCoord: 1` and would sample the atlas on the first
  uv set; use LightmapGI or SSAO in Godot).
- A marker's yaw 0 faces +z, which is the back of a Godot node (its
  forward is -z): the transform is the same, read `basis.z` for its facing.
