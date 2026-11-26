# Procedural materials

Materials are node graphs (SDFs, noise, maths, colour) evaluated on the CPU
in parallel and baked into PBR textures. Same app, same server, same
structures as models: nodes are upserts by name, batches are atomic, params
work, and every material has its own history tree.

- `material_ops(material, ops, params?, domain?, tile?, resolution?, expect?)`
  edits (or creates) a working material. Each node is `{"type", "name",
  ...}`; `{"type": "delete", "name"}` removes one. Inputs are numbers,
  `[r, g, b]`, `"#rrggbb"` (sRGB, stored linear) or another node by name
  (`"bricks"`, or a port: `"bricks.id"`). Coordinate inputs (`coords`)
  default to the sample's uv in meters, so warping is "transform the coords,
  feed them on". `describe_op("nodes")` lists every node type with an
  example; `describe_op("node:<type>")` gives one schema.
- **Domains.** `tile` (default): a seamless square `tile` meters wide.
  Noise lattices snap to a whole number of cells per tile so it repeats
  exactly; on parts it is mapped by metric uvs, so a 0.25 m brick is 0.25 m
  everywhere. `object`: evaluated at points on the surface (`input` of
  `position` / `normal`, object space, meters): 3D noise and SDFs run through
  the solid and wrap edges with no seams. Each part gets a packed atlas
  (box-unwrapped islands at one texel density, dilated gutter). `card`:
  one `tile`-meter image (like a tile, coordinates in meters, top row =
  v 0) laid once over each part's own 0..1 uvs instead of remapped: leaf
  cards, signs. One shared texture, like a tile. `decal`: the same image
  space as `card`, for `decal` items (which accept `card` materials too).
- **Node families.** Coords: input, transform, combine, split, polar,
  kaleido, warp, tile_grid (ports local / cell / id / edge), repeat,
  triplanar. 2D SDFs: circle, rect, segment, polygon, star, hexagon, arc,
  rings, text (letters: `{"type": "text", "name": "t", "text": "OPEN",
  "size": 0.2, "center": [0.5, 0.5], "font": "bold"}`, `size` = capital
  height in meters, `\n` for lines, `align`, `line_spacing`,
  `letter_spacing`, `angle` degrees; the same fonts as the `text` item;
  feed it to `mask` for the fill, `mask` glow for an outline: signs,
  labels, graffiti decals, stencils on crates), svg (see below). 3D SDFs: sphere, box, torus, capsule, cylinder, plane. SDF ops:
  sdf_op (union / intersect / subtract and smooth_ versions), sdf_round,
  onion, displace, mask (fill / stroke / glow, anti-aliased). Noise: noise
  (value / gradient; fbm / ridged / turbulence), worley (f1 / f2 / edge /
  id). Maths and colour: math, remap, random (per id), color, ramp, hsv,
  blend, levels, gradient (linear / radial; 3-value points read object
  position, a fade across a part), facing (cosine to a direction: dust on
  tops, moss on one side), image (a PNG, `size` meters per repeat; ports
  colour / alpha / value). Neighbourhood: blur, dir_blur (smear along an
  angle or a `flow` field: streaks, brushed metal, hair),
  normal_from_height, ao_from_height, curvature, dilate, islands (connected
  regions of a mask: `id` per island for `random`, `area`).
- **`svg`**: an SVG drawing as an exact signed distance field, like the
  other 2D SDFs (mask, sdf_op, onion, displace, heights all work):
  `{"type": "svg", "name": "logo", "svg": "<svg viewBox=...>...</svg>"` (or
  `"path": "art/logo.svg"`, project-relative) `, "size": [w, h]` meters
  (or the width alone), `"center"` (default [0.5, 0.5]), `"angle"`,
  `"fit": "contain" | "stretch"}`. usvg parses it (transforms, groups,
  shapes, paths, strokes as outlines, text as Fira glyphs; fill-rule
  honoured). Ports: default = union of every filled shape; one per element
  `id` (`logo.ring`); `logo.colors` (the SVG's own fill colours, linear,
  anti-aliased: a full-colour decal) and `logo.alpha` (coverage). Parsed
  once per content hash, edges bucketed in a grid. Gradients read as
  their average colour; images, filters, masks, clips and dashes are
  ignored.
- **`project`**: planar coordinates on one side of a part, so 2D artwork
  (svg, text, shapes) lands on a named face in the object domain without
  knowing the atlas: `{"type": "project", "name": "front_uv", "face":
  "front" (back/left/right/top/bottom; or "normal": [x, y, z], "up"?),
  "at": [a, b], "depth"?: m}`. `at` is the artwork origin in the face's
  cross axes (front/back [x, y], left/right [z, y], top/bottom [x, z]; the
  part's own local axes: a box primitive is centred on its origin, so a
  translated hull 0.9 tall has its front's centre at [0, 0]). Output: vec2 meters, `at` at [0, 0], +x to the right
  seen from outside, +y down, so give the artwork `"center": [0, 0]`;
  `front_uv.facing` is 1 on surfaces facing that way (cosine `lo` 0.6 ..
  `hi` 0.85) and 0 elsewhere and further than `depth` behind the part's
  front. A badge on a tank's front (object domain):
  `material_ops("tank_paint", domain: "object", ops: [{"type": "project",
  "name": "fp", "face": "front", "at": [0, 0.1], "depth": 0.05}, {"type":
  "svg", "name": "badge", "coords": "fp", "center": [0, 0], "size": 0.4,
  "svg": "<svg viewBox=\"0 0 100 100\"><circle cx=\"50\" cy=\"50\"
  r=\"45\" fill=\"#fff\"/><path fill=\"#c22\" d=\"M50 12 L61 40 L90 40
  L66 58 L75 88 L50 70 L25 88 L34 58 L10 40 L39 40 Z\"/></svg>"}, {"type":
  "math", "name": "m", "op": "mul", "a": "badge.alpha", "b": "fp.facing"},
  {"type": "noise", "name": "n", "size": 0.3}, {"type": "ramp", "name":
  "paint", "input": "n", "stops": [[0, "#3e4a2c"], [1, "#55603a"]]},
  {"type": "blend", "name": "col", "a": "paint", "b": "badge.colors",
  "opacity": "m"}, {"type": "output", "name": "o", "base_color": "col",
  "roughness": 0.6}])`.
- **`scatter`** is a tile sampler: shapes (ellipse, circle, rect, feather,
  leaf) strewn on a jittered grid of `cell` meters, `per_cell` each, with
  random `scale`, `angle_jitter` and `density`, pointing along `angle` or a
  `flow` field, stacked `layering: "random"` or `"flow"` (upstream ones on
  top: feathers, scales, shingles). The top instance drives the ports:
  `mask`, `local` (-1..1 inside it, base to tip along y: draw any detail
  with other nodes on these coords), `id` (per instance, for `random`),
  `height` (a dome) and `edge` (meters inside). Tileable.
- **Mesh-aware masks** (object domain on a part): `mesh_ao` (occlusion
  from the part's own mesh: creases and hollows darken) and
  `mesh_curvature` (0.5 flat, ports `convex` for edge wear and `concave`
  for dirt in crevices), from a voxelized copy of the part and the visible
  parts near it (a wing darkens the flank under it); without a mesh (tile domain, the
  preview sphere) they read neutral.
- **Forgiving node inputs**: `output` takes `metalness` for `metallic`;
  `noise` takes `noise` for `mode` (and `kind: "ridged"` is read as the
  fractal mode; `kind` is value or gradient); `remap` takes `from: [lo,
  hi]` / `to: [lo, hi]` or `in_min` / `in_max` / `out_min` / `out_max` (a
  bare number for from / to is an error); `circle` takes `center` and
  `radius` (an `arc: {center, radius}` is accepted); `color` takes `color`
  for `value`; `mask` takes `input` for `sdf`; `dir_blur` has no
  `iterations` (use `length`). A node missing a required field gets its
  example payload in the error. Card and decal space is **y-down** (v 0 =
  top) while the `text` node draws upright: an up-pointing `polygon` arrow
  has its tip at the smallest y.
- **`output`** names the channels: base_color, roughness, metallic,
  height (meters; the normal is derived from it, times normal_strength,
  unless `normal` is given), ao, emissive, opacity. Missing channels use
  defaults. Normals are in the glTF convention (+y up the image). Whole-
  surface factors (numbers): `clearcoat` / `clearcoat_roughness`
  (lacquer, wet), `anisotropy` / `anisotropy_rotation` (degrees; brushed
  metal, hair, satin), `transmission` (light through leaves, paper, skin),
  `specular_transmission` + `thickness` + `ior` (glass, water). They show
  in the preview and export as KHR_materials_clearcoat / anisotropy /
  diffuse_transmission / transmission / volume / ior.
- **Alpha.** `opacity` below 1 renders by `alpha`: `"mask"` (a cutout:
  each texel fully there or not at `alpha_cutoff`, default 0.5; leaves,
  grass, chain-link, lace; stays in the opaque pass, casts cut-out
  shadows, needs no sorting), `"blend"` (see-through: glass, smoke, ghosts)
  or `"opaque"`. Default: mask when the opacity is nearly all 0 or 1, else
  blend. `double_sided` (default on for mask) draws both faces, so a card
  shows from behind; on a closed solid (a sign plate with `uvs` `face`)
  a mask cutout stays single-sided, so the back doesn't show through.
  Exports write glTF `alphaMode` MASK / BLEND,
  `alphaCutoff` and `doubleSided`; a mask texture's mipmaps keep its
  coverage, so foliage doesn't thin out and vanish at a distance.
- **Layers**: `output` takes `over: [{"output": "dirt_out", "mask":
  "dirt_m"}]`, each a whole other `output` node blended in (every channel)
  where its mask is 1, in order: moss over stone, rust over paint, mud over
  feathers. The main output is the one no other output layers in.
- **`expect`** checks: `tileable` (bakes the tile shifted by half a tile and
  compares: a non-periodic node fails, naming the channel), `ranges` (finite,
  0..1), `albedo` (non-metal luminance in 0.02..0.9 at the 1st / 99th
  percentile, or your range), `normals` (unit, facing out), `coverage`
  ([{node, min, max}]: share of the area where that node is above 0.5;
  a failure gives the node's 5/50/95% values and, for a node with an
  `input` (levels, remap), the input value that would hit the wanted
  coverage, so the threshold is one edit away).
- `material_view(material?, node?, channels?, size?, diameter?, detail?)`: one
  image, rendered on the CPU from the bake that gets exported (works
  headless): a lit sphere plus channel thumbnails, or one node's output
  (`"g.id"`), with the value range in the text.
- `material_catalog(action, ...)`: `list` (the library), `working` (this
  session's materials), `describe`, `save` (name, description, tags,
  prompt), `load`, `delete`. The library is `materials/` next to the
  prefabs, shared by every model. Working materials autosave apart from the
  model (`materials/session.json`).
- **On parts:** `{"type": "material", "name": "wall", "pbr": "bricks",
  "params": {"mortar": 0.012}, "scale": 1.5}`. The graph is embedded in
  the op, so models and prefabs bake identically without the library; a
  `material_ops` edit re-embeds the new graph in every item of the active
  model that uses it (one history step), so parts follow without re-issuing. Materials follow lineage like
  colours; the uv mapping runs after the whole graph, so cut faces and copies
  are mapped too (it replaces any `uvs` projection on those parts). Factors
  become 1 (white) unless given, then multiply the textures.
- **Export:** `export_model` writes the baked textures (base colour,
  metallic-roughness, normal, occlusion / emissive when driven) and a
  TANGENT attribute. **Prefer `.gltf`** (the default when `format` is
  omitted, unless the path ends in `.glb`). Its textures are **shared KTX2
  files**: GPU block-compressed (BC7 colour, BC5 normals, BC4 occlusion)
  with a full mip chain and zstd supercompression, which Bevy's default 3D
  features (`ktx2`, `zstd_rust`) load as they are and keep compressed on the
  GPU (about a quarter of a PNG's memory, no shimmer at a distance). Each
  goes to the `textures/` folder at the root of the `assets/` folder the
  model is exported into (else `textures/` beside the .gltf), named by
  material, channel and a hash of its content
  (`stone-base_color-1a2b....ktx2`), and the .gltf points at it by uri.
  Every model using the same bake points at the same file, so Bevy's asset
  server loads it once. `.glb` (one self-contained file) packs the same KTX2
  into its buffer (`mimeType: image/ktx2`, which Bevy's loader reads): still
  GPU-compressed, but each .glb has its own copy, so prefer .gltf for a kit.
  `optimize: {"textures": ...}` overrides: `"ktx2"` (shared files), `"png"`
  (shared PNGs), `"embed"` (KTX2 in the buffer), `"embed_png"` (PNG in the
  buffer, for viewers without KTX2 such as Blender). Only **tile** materials
  (the default domain: metric, repeating) can be shared across models; an
  **object** bake is fitted to one mesh, and the export report counts
  those. Use tile materials for anything a kit reuses (stone, bark, leaves,
  metal) and keep object materials for one-off hero parts.
- **History:** `undo` / `redo` / `jump` / `history` take `doc:
  "material:<name>"` (or `"material"` for the active one).
- **Bake cache:** bakes are cached by key (material, params, and for
  object materials the part's mesh and neighbours), least recently used
  evicted past 1 GB; the preview and exports bake all parts in parallel
  first, so only parts whose key changed re-bake after an edit.
- **Parallelism:** a level's nodes run in parallel, every kernel is
  data-parallel over samples (rayon), results are deterministic at any
  thread count, and a memo cache keyed by node + input keys + domain
  re-runs only what a tweak touches (results report "N computed").
