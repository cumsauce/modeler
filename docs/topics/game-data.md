# Game data: colliders and markers

**Game data (colliders, markers):**

- `collider` rules give parts collision shapes:
  `{"type": "collider", "name": "level", "parts": ["*"], "shape": "box"}`
  then narrower rules (later wins per part): `mesh` for static level
  geometry that needs its real shape (ramps, arches), `hull` for props,
  `body: "dynamic"` for things that fall and get pushed (hull or box only),
  `sensor: true` for triggers, `none` to exempt (decals, foliage).
  A static `box` is the part's own box turned with it (a tilted ramp or
  roof slope gets a tight box; the collider node carries the rotation);
  moving bodies get a world-aligned box. Rules skip hidden parts unless
  they say `hidden: true`: then a hidden part exports as a collider node
  with no render mesh, so invisible blockers and trigger volumes live in
  the model, not in game code (`{"type": "collider", "name": "blockers",
  "parts": ["block_*"], "shape": "box", "hidden": true}`, with the
  `block_*` boxes hidden; `box` / `hull` only). The window draws them as
  gizmos too. Hidden parts don't come into prefab instances, so a scene
  using the prefab gets its blockers from the prefab's own glTF.
- `marker` items are named points for game code: spawns, waypoints, item
  spots, cameras, trigger zones (`size` = a box volume). `yaw` faces them
  (degrees about +y, 0 = +z), `data` carries any JSON (`{"team": 1}`),
  `attach` makes them ride a part's moves. `preview: "sp-warehouse-exterior"` draws that catalog
  prefab at the marker (standing on the ground, hidden terrain too, turned
  by yaw) in the window, grid and agent views only, never exported: what
  game code builds there, so a map isn't just outlines.
- Both come along with prefab instances (`b1.grab`; the prefab's rules
  apply first, so the scene can override: `{"parts": ["b2.*"], "shape":
  "box"}`), follow moves, and draw as gizmos in the window and agent views
  (markers magenta, colliders green static / orange moving / blue sensor).
- Export (any `optimize`): a `box`/`hull` collider is a node
  `<part>.collider` at the part's centre with `extras.collider`
  `{shape: "cuboid", half_extents} | {shape: "convex_hull", points}` plus
  `body` and `sensor`; a moving body's render node is its child so they
  move together. `mesh` sets `extras.collider = {shape: "trimesh"}` on the
  part's own node. Markers are empty nodes with `extras {marker: kind,
  size?, data?}`. The report counts both. With `merge`, keep moving bodies
  and mesh colliders out via `merge_keep` (the report warns).
- In Bevy, enable the glTF extras (`GltfExtras` comes with the scene) and
  map them, e.g. with Avian:

  ```rust
  fn colliders(q: Query<(Entity, &GltfExtras), Added<GltfExtras>>, mut c: Commands) {
      for (e, x) in &q {
          let Ok(v) = serde_json::from_str::<serde_json::Value>(&x.value) else { continue };
          let Some(col) = v.get("collider") else { continue };
          let shape = match col["shape"].as_str() {
              Some("cuboid") => { let h = &col["half_extents"];
                  Collider::cuboid(2.0 * h[0].as_f64().unwrap_or(0.5) as f32,
                                   2.0 * h[1].as_f64().unwrap_or(0.5) as f32,
                                   2.0 * h[2].as_f64().unwrap_or(0.5) as f32) }
              Some("convex_hull") => Collider::convex_hull(
                  serde_json::from_value::<Vec<Vec3>>(col["points"].clone()).unwrap_or_default())
                  .unwrap_or_else(|| Collider::sphere(0.1)),
              _ => { c.entity(e).insert(ColliderConstructorHierarchy::new(
                  ColliderConstructor::TrimeshFromMesh)); continue }
          };
          let body = match col["body"].as_str() {
              Some("dynamic") => RigidBody::Dynamic,
              Some("kinematic") => RigidBody::Kinematic,
              _ => RigidBody::Static,
          };
          c.entity(e).insert((shape, body));
          if col["sensor"].as_bool() == Some(true) { c.entity(e).insert(Sensor); }
      }
  }
  ```

  Markers the same way: read `extras.marker` and spawn your own
  components at the node's `Transform`.
