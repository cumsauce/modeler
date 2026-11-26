# `variant`

**Variants (cosmetic skins and outfits):** alternate looks for one model
(red / blue team, clean / rusty, casual / formal) that game code switches
at runtime. Geometry never changes; the base model is the variant
`default`.

- **Outfits**: `show` / `hide` (part names or `*` globs) make a variant
  an outfit: `{"type": "variant", "name": "casual", "show": ["shirt",
  "jeans"], "hide": ["armor_*"]}` (hide the outfit's pieces in the base
  with `hide_part`). The variant re-derives with those visibility edits
  after every op, so garments' covered-skin masks follow the outfit.
  `agent_views(variant / variants)`, `verify materials` per variant and
  the export see it. Export: every part some variant shows is exported,
  and its node carries `extras.variants_visible: {"default": false,
  "casual": true, ...}`; only garments visible in every variant hide skin
  in the exported body (one mesh serves every outfit).
  `modeler_bevy::ModelerPlugin` reads it into `ModelerVariantVisibility`
  and sets each node's `Visibility` from the `ModelerVariant` above it
  (default when none).

- `{"type": "variant", "name": "red_team", "materials": {"body*": {"pbr":
  "paint", "params": {"hue": 0.0}}, "trim": {"color": "#c33", "flat":
  true}}, "style": {"colors": {"primary": "#c33"}}}`. `materials` keys are
  part names or `*` globs; values are a `material` item's fields (minus
  name / input; `flat: true` drops the base pbr). Globs apply first, then
  exact names, so an exact name wins. `style` patches style tokens for the
  variant: every item that reads tokens re-resolves under it (like a
  prefab's style patch). Either or both.
- A variant is applied after the whole graph (`src/variant.rs`): the parts
  derive as usual, then each matched part's material is replaced, so a
  variant changes exactly the parts it names (not copies made from them:
  name `"trim*"` to take those). A look that matches no part is a
  `scene_summary` warning.
- See it: `agent_views(variant: "red_team")`, or `variants: true` (or a
  list) for the views once per variant in one tiled grid (labelled
  `[red_team]`). `set_camera(variant: "red_team")` makes the window show it
  (per document; `"default"` or `""` goes back), and the window's 3D view
  has a Skin chip that cycles them. `scene_summary` lists the variants.
- Check it: `verify` / `expect` `materials` runs on the base look and every
  variant (one `materials (variant x)` line each); `variant: "x"` limits it
  to one.
- Export: glTF `KHR_materials_variants` (root `extensions` lists
  `default` first, then each variant; each primitive's `mappings` give its
  material per variant), every variant's materials written once (shared
  ones deduplicated), and node `extras.variants: {variant: material
  index}` for engines without the extension. Instancing and merging are
  off in a model with variants (each part needs its own primitive).
- Prefabs: a `prefab` item takes `variant: "red_team"` (the instance's
  parts wear it in the modeler), and a `.bsn` export writes
  `modeler_bevy::ModelerVariant { name: "red_team" }` on that entity (and
  `variant` in `<stem>.map.json`); the prefab's glTF carries all its
  variants.
- In Bevy (whose loader ignores `KHR_materials_variants`), `ModelerPlugin`
  reads `extras.variants` into `ModelerVariants`; put `ModelerVariant` on
  the scene root (or any ancestor) to pick a look and change it to switch:

  ```rust
  let root = commands.spawn(WorldAssetRoot(assets.load(
      GltfAssetLabel::Scene(0).from_asset("models/tank.gltf")))).id();
  commands.entity(root).insert(modeler_bevy::ModelerVariant::new("red_team"));
  // later: commands.entity(root).insert(ModelerVariant::new("blue_team"));
  ```

  It swaps each part's `MeshMaterial3d<StandardMaterial>` to the file's
  `Material{i}/std` handle. The viewer example cycles them with V.
