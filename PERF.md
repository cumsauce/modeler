# Performance findings

A review done in three parts: the preview window, the core library and the server path. The fixes have since landed; see **Status** at the end.

**Baseline.** `cargo test --test perf` passes every budget comfortably:

| Case | Time |
|---|---|
| 289-part build | 104 ms |
| One edit | 4 ms |
| Cold bake of 82 parts | 368 ms |
| Cached re-bake | 0.7 ms |
| verify (grounded + supported + collisions) | 262 ms |
| Autosave, long history | 95 ms |

The perf test has no map case, and maps are where the slowness lives. **Add a map perf case first** (a 220 m terrain and about 12 prefab instances with real histories), timing `refresh`, `markers`, `verify`, a window rebuild and export. Then measure the fixes below against it.

## Top 10, ranked across all three areas

| # | Area | Problem | Fix | Impact |
|---|---|---|---|---|
| 1 | server | Every call takes 4 main-loop round trips (`begin_call`, tool, `end_call`, `close_idle`, each awaited; `tools.rs:1604-1628`). In reactive mode nothing wakes the loop, so that is up to about 130 ms per call with the grid on, and about 65 ms otherwise. | Fold them into one `Cmd::Exec` (or send the bookkeeping without awaiting), wake winit with an `EventLoopProxy` on send, and move `close_idle` to a timer system. | high, cheap |
| 2 | server | The Python daemon serialises every call from every agent behind one global lock (`mcp_driver.py` ~380/452/475). One 15 s render blocks everyone's `scene_summary`. | Multiplex: a fresh JSON-RPC id per request, one writer (short lock), one reader thread routing responses by id. Pass response bytes through untouched; today base64 PNGs go through `json.loads`/`dumps` twice. | high |
| 3 | core | `load_prefab_file` (`catalog.rs:1165`) re-reads the JSON, parses the history sidecar and runs `restore_parts()` (a full `refresh`) on every history node, per lookup. It is called from graph eval, `game::markers`/`rules` (recursively, every call), `light.rs`, `scene.rs`, `commands.rs` and `spec.rs`. | Add an instancing loader that skips history, plus a process-wide `name -> (mtime, Arc<Model>)` cache. Compute markers, rules and lights once per prefab, ideally cached on the derived model. | high on maps |
| 4 | preview | `rebuild_scene` (`app.rs:237-305`) despawns every part and rebuilds and re-uploads every mesh on any version bump, with mikktspace tangents on textured parts. On a 97k-tri terrain the tangents alone take tens of ms, and the main-world mesh copy doubles memory. | Keep a persistent `part name -> (Arc ptr, Entity, Handle<Mesh>)` map and respawn only changed parts. Use `RenderAssetUsages::RENDER_WORLD`. | high |
| 5 | server | A document switch, which happens on nearly every call with several agents, rewrites the whole history journal (the `extends` check fails on a different tree) and all of `open_docs.json` with every document's history (`catalog.rs:476-525, 566, 656`). | Per-document journals. `open_docs.json` without histories. A switch with no edit shouldn't mark the model dirty. | high with several agents |
| 6 | preview + core | No mesh or material sharing. Scatter copies, prefab instances and arrays each `meshes.add` their own copy, and every flat-colour part gets a fresh `StandardMaterial` per rebuild (`app.rs:491-509`), so Bevy can't batch. | Cache `Handle<Mesh>` by `Arc` pointer or geometry hash, and flat materials by `(colour, roughness, metal)`. When materials are shared, the selection highlight needs a per-part clone (`panel.rs:8-33` already pulses every part sharing a pbr material). | high on maps |
| 7 | preview | `marker_previews` (`app.rs:797-861`, also run per grid tile) loads prefabs from disk, refreshes them and deep-clones the terrain to ray-cast it on every rebuild. | Use the cache from #3, ray-cast the existing Arcs (or the heightfield), cache snapped positions per version, and share mesh handles across instances. | high on maps, cheap |
| 8 | server + core | `sync_prefabs` on each switch resolves every instance separately: a disk read, or a deep clone of an open doc out of `OPEN_DOCS` (`catalog.rs:1028`), plus JSON hashing. | `Arc<Model>` in `OPEN_DOCS`, resolve each distinct prefab once, keep a version or content hash on `Model` instead of hashing JSON, and skip the sync when no source changed. | med-high |
| 9 | core | Verify and ray-casting have no acceleration. `ray_cast` (`contact.rs:923`) re-transforms every vertex of every visible part per ray. The grounded BFS (`contact.rs:602`) is O(n²) full contact tests, and `Body::new` rebuilds a Manifold per part per verify, terrain included. | AABB cull, a cached per-part BVH (keyed by Arc / content hash) with rays moved into local space, one `Contacts` pass with a broadphase, and Manifold/volume cached across verifies. | med-high |
| 10 | preview | Grid tiles duplicate the whole rebuild and re-run `clone_without_history` + `refresh` for documents restored with ops only (`grid.rs:518-600`). | Share #4's per-document mesh cache, and cache the derived model per `(version, history length)` or store the parts back into the session. | med |

## Smaller items

### Core
- **Op memo key (`graph.rs:131`).** It serialises every op to JSON on each evaluate, hits included. That is megabytes for `tri_mesh` and terrain with long `paths`. Cache each op's key on the op.
- **`Named` lookups.** `Named` is a `Vec` searched linearly (`graph.rs:160-176, 1269`). Add a `HashMap` index.
- **Op cache eviction.** The op cache clears entirely when full (`graph.rs:180`), and so does `PbrCache` at 256 entries (`app.rs:456`), which then re-bakes every texture. Use LRU or evict-unused instead.
- **`bake_key` (`material/pbr.rs:114`).** It hashes every vert of object-material parts and re-serialises the graph per part. Store a content hash on `Part`, and hash the graph once per edit.

### Preview, per frame
- **`highlight_selected`** loops over every part even with nothing selected.
- **`draw_game_gizmos`** re-emits every marker and collider line each frame. Use a retained `GizmoAsset`.
- **The outliner** builds a doc-names string every frame to compare. On a change it re-reads `catalog::list` from disk and rebuilds every row.
- **`drawn()`** is O(ops) per part. Pass a terrain-name set instead.
- **Idle rendering.** With the grid off, the window renders continuously with TAA and SSR while nothing moves. Go reactive when idle.

### Not problems
- Graph hashing doesn't hash mesh content.
- Parts are `Arc` through evaluation.
- The `catalog.json` index isn't read per call.
- `persist_fingerprint` is cheap.
- The `scene_summary` floating warning is microseconds.

## Suggested order

1. Map perf case, then #1 (call round trips), then #3 + #7 (prefab cache).
2. #4 + #6 + #10: one diffed part-to-entity map with shared handles, used by both the window and the grid.
3. #2 (daemon multiplexing) and #5 + #8 (switching cost). These matter most with several agents.
4. #9 (BVH and verify broadphase), then the smaller items.

## Status (fixed)

| # | What changed |
|---|---|
| map case | `tests/perf_map.rs`. Build 8 ms, a marker edit 2 ms, `markers()` 0.2 ms, 20 rays 3 ms, verify grounded 190 ms cold / 60 ms warm. |
| 1 | Each call awaits one main-loop trip. `begin_call`, `end_call` and `close_idle` are sent without waiting, and every send wakes winit (`bridge::WAKE`). Five calls went from 0.68 s to 0.37 s. |
| 2 | The daemon sends calls concurrently. A reader thread routes responses by id, and the lock only guards respawning. A `scene_summary` takes 0.15 s during another agent's 0.9 s render. |
| 3, 7, 8 | `catalog::prefab_instance` returns a cached `Arc<Model>` (no history, keyed by mtime), and `OPEN_DOCS` holds `Arc`s. It is used by instancing, markers, rules, lights, scene export, `sync_prefabs` and marker previews. Previews ray-cast the terrain in place. |
| 4, 6, 10 | `PbrCache` shares meshes by content and flat materials by look across rebuilds, instances, copies, grid tiles and agent views. Pbr materials are evicted least recently used first. The selection pulses a per-part copy. Entities are still respawned, which is cheap. Meshes keep their main-world data because picking ray-casts them. |
| 5 | One journal per open document (`session.history.<doc>.jsonl`), and `open_docs.json` without histories. A journal is appended to across restarts. |
| 9 | Rays use a cached world-space BVH. Manifold solids are cached across verifies. `floating` uses the culled, parallel contact pairs. |
| small | Op memo keys and bake keys hash serde output as it streams. The op cache evicts its least recently used half. Geometry hashes are memoised. `drawn()` is a set lookup. The outliner hashes names. The window idles at 4 fps after 2 s of stillness. |

**Not done**, because the gain is too small at current sizes:
- a `Named` hash index (lookups are linear, a few ms at 2k parts);
- a retained gizmo asset (gizmos are already cached per version);
- moving `close_idle` to a timer (it already runs at most once a minute).

Also fixed on the way: agent views rendered a stale scene while the grid was up. The window scene now rebuilds while shots are pending.
