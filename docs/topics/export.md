# Game export

**Game export (`export_model(..., optimize)`):** `optimize: "game"` (or an
object of flags over it; `"none"` / omitted = plain) prepares the model for
Bevy and returns a before -> after report (tris, verts, meshes, materials,
batches, bytes):

- `dedup_materials` (on): identical looks share one glTF material, so Bevy
  shares the `StandardMaterial` handle and can batch.
- `cull_contacts` (on): drops triangles whose samples, nudged outward, land
  inside another visible solid (bottoms on the deck, insets in frames).
- `visible_from: [{min, max}]` (opt-in): drops every triangle of a closed
  solid or one-sided sheet that faces away from all of those camera boxes
  (an exact plane test, no sampling), e.g. an interior drops its walls'
  outer faces. Thin visible parts are kept, open single-layer meshes (leaf
  cards) are never touched, and copies that cull identically still
  instance. `visible_from_rays: true` also drops triangles no ray reaches
  from `view_samples` (default 256) points in the boxes (blended parts
  don't block); it can cull thin visible surfaces and breaks instancing for
  the parts it changes. The report gives the culled count in `.gltf`,
  `.glb` and `.bsn` exports.
- `instance` (on): translated copies share one mesh with their own node.
- `merge: "by_material"` (opt-in): one mesh per material; `merge_keep:
  ["door*"]` keeps parts the game needs by name.
- `weld`, `small_indices` (u16), `orm` (AO packed into metal-roughness R),
  `emissive_strength` (KHR_materials_emissive_strength) are on in `game`.
- `lods: [0.5, 0.2]` (opt-in): every part with at least `lod_min_tris`
  (300) triangles gets simplified copies `<part>.lod1`, `.lod2` (quadric
  simplification that keeps seams and uvs, so they reuse the full part's
  textures) as sibling nodes. Skinned parts get LODs too: half-edge
  collapses keep each surviving vertex's own skin weights, morph targets
  and corrective offsets, a cost term (weight change squared times edge
  length^4) keeps the density where weights change fastest (the joints),
  and each LOD node is skinned to the same joints with its own
  JOINTS_0 / WEIGHTS_0 and carries the correctives (and `shapes`) at
  every level (sparse, so cheap). The human walk stays at 1.90x stretch
  on 26.8k / 13.4k / 5.4k triangle levels. Each node's `extras.lod = {level, near,
  far}` gives the camera distances (m) it is meant for: `lod_distances:
  [20, 60]`, or by default 15x and 45x the part's size. Instanced copies
  share the LOD meshes. In Bevy, put a `VisibilityRange` from those
  numbers on each node (`start_margin: near..near`, `end_margin:
  far..far`, or with a few meters of crossfade): `modeler_bevy` does this
  from `extras.lod` (`ModelerLods`).
- `ao: {resolution: 1024, distance: 1.0, samples: 64, skip: []}` (opt-in;
  `ao: true` = those defaults): **baked ambient occlusion** for rooms and
  levels built from big quads with shared tile materials. Every exported
  primitive gets a second uv set (`TEXCOORD_1`): box-unwrapped islands at
  one texel density in **one atlas per glTF** (the object-domain unwrap and
  shelf packing, 3-texel dilated gutter; vertices split at island borders,
  uv0 unchanged). Each texel traces `samples` cosine-weighted hemisphere
  rays against all the export's opaque geometry; hits beyond `distance`
  meters count as open (so walls darken where they meet the floor, floors
  under a counter), then a 3x3 in-island blur and the gutter dilation. The
  atlas is one BC4 KTX2 (or PNG with `textures: "png"`) named
  `<gltf stem>-ao-<hash>` in the shared `textures/` folder, and **every
  material** gets `occlusionTexture: {index, texCoord: 1}` (replacing the
  tile AO an `orm` texture carried; Bevy applies it to indirect light only).
  Instancing is off with `ao` (each copy needs its own patch). `skip`
  (part-name globs: terrain, huge ground) and rigged parts still occlude but
  get no islands, reading a white texel, as do LOD copies and decal quads.
  Parts without a texture get uv0 = uv1 (glTF wants uv sets from 0). A
  `.bsn` export bakes the map's own geometry as one atlas and each prefab
  glTF from itself alone. The report gives atlas size, islands, texels,
  texels/m, triangles, rays and time.
- Nodes carry `extras {part, material, comments, lod?}` for game-side lookup.
