# `lsystem`

- **`lsystem`** grows branching structures: trees, bushes, ferns, weeds,
  roots, coral, antlers, cracks. A `preset` gives a ready plant at real size
  (`tree` ~4.5 m broadleaf, `pine` conifer, `bush` ~1 m shrub, `fern` five
  arching fronds, `weed` tuft, `roots` growing down); every field overrides
  it, and `seed` varies the individual (same seed = same plant: a forest is
  a `repeat` over seeds). Or write your own: `axiom` + `rules`
  (`{"X": "F[+X][-X]FX", "F": "FF"}`; a list of choices is stochastic,
  `[{"p": 2, "to": ...}, ...]` weighted) for `iterations` (<= 12) rewrites,
  then a 3D turtle draws it: `F`/`G` draw a segment, `f` moves, `+`/`-`
  turn, `&`/`^` pitch, `\`/`/` roll, `|` turn back, `[`/`]` a side branch
  (thinner by `radius_scale`, shorter by `length_scale`, rolled by
  `branch_roll`, 137.5 = phyllotaxis), `!` thins, `L` places a leaf; other
  letters only rewrite. `jitter` (degrees) and `length_jitter` add natural
  variation, `tropism` + `tropism_strength` bend growth toward a direction
  (`[0, -1, 0]` droops, `[0, 1, 0]` reaches up), `taper` thins along a
  branch. Each unbroken branch is ONE smooth closed tube (`sides`), so no
  gaps at bends; branches start inside their parent. Leaves (`leaf_size`
  [width, length]) go to `name_leaves`: give the two parts separate
  materials (`"*_leaves"` patterns see them). Leaf look: `leaf_angle`
  (pitch out from the stem), `leaf_curl` (tips droop), `leaf_fold` (V
  along the midrib), `leaf_stalk` (petiole, meters).
  **Leaves are cards by default** (`leaf_mode: "card"`), the way games draw
  foliage: each leaf is a flat quad (2-6 triangles with curl and fold,
  `leaf_cross: true` adds a second card at 90 degrees for volume) with uvs
  0..1 over it, and the leaf's shape comes from an **alpha cutout**
  material, not from polygons. Give `name_leaves` a `card`-domain material
  whose output sets `opacity` (1 on the leaf, 0 around it); a part of cards
  without one warns, since every leaf then draws as a rectangle. The image's
  top (v 0) is the leaf tip, its bottom the base at the twig; it is
  stretched over the card's width x length. A leaf (`tile` 0.1 is the
  image's size in meters, the coordinate space of the shapes):
  `material_ops("leaf_card", domain: "card", tile: 0.1, resolution: 256,
  ops: [{"type": "circle", "name": "l", "center": [0.0332, 0.05], "radius":
  0.0518}, {"type": "circle", "name": "r", "center": [0.0668, 0.05],
  "radius": 0.0518}, {"type": "sdf_op", "name": "lens", "a": "l", "b": "r",
  "op": "intersect"}, {"type": "mask", "name": "shape", "sdf": "lens"},
  {"type": "segment", "name": "rib", "a": [0.05, 0.004], "b": [0.05,
  0.098], "width": 0.0015}, {"type": "mask", "name": "vein", "sdf": "rib"},
  {"type": "noise", "name": "n", "size": 0.02, "octaves": 3}, {"type":
  "ramp", "name": "green", "input": "n", "stops": [[0, "#2f5a1c"], [1,
  "#5f8f2e"]]}, {"type": "blend", "name": "col", "a": "green", "b":
  "#a8c070", "opacity": "vein"}, {"type": "output", "name": "o",
  "base_color": "col", "roughness": 0.6, "opacity": "shape",
  "transmission": 0.3}])`, then `{"type": "material", "name":
  "oak_leaves", "pbr": "leaf_card"}`. For dense crowns, draw **clusters**:
  paint several leaves (or a twig of them) into one card image and raise
  `leaf_size`, so one card stands for many leaves. A card material is one
  shared texture (like a tile), so a scattered forest reuses it.
  `leaf_mode: "solid"` models each leaf as a closed blade in its
  `leaf_shape` outline (ellipse, lance, round, heart, diamond, needle,
  maple; ~60 triangles a leaf, no texture needed): for close-up hero
  plants, prints, or when the user wants geometry. Leaf cards are open
  sheets on purpose: `validate`, `thickness` and `self_intersections` skip
  them. Models saved before cards existed keep their solid leaves.
  **Parametric** rules carry numbers on modules, so each branch knows its
  own length, width or age: axiom `"A(1, 0.08)"`, rules
  `{"A(l, w) : l >= 0.2": "!(w)F(l)[&(40)A(l*k, w*0.6)]/(137.5)A(l*0.8, w*0.8)",
  "A(l, w) : l < 0.2": "L(1)"}` with `constants: {"k": 0.6}` (values may
  be `"$param"`). Expressions: `+ - * / % ^`, comparisons, `&& || !`,
  `min max abs sqrt sin cos floor`. The turtle reads a module's first
  number: `F(len)` / `f(len)` meters, `+(deg)` and every turn, `!(radius)`
  sets the radius, `L(scale)` scales the leaf. A symbol's rules with a
  condition are tried first (in key order), then the one without, so a bare
  `A(l)` works as the fallback; first match wins; a rule only matches modules with the same number of
  parameters. Set `radius_scale` / `length_scale` to 1 when the rules size
  branches themselves. Runaway growth
  (over 400k symbols or 40k segments) is refused with a hint. Params work
  in every number (`"length": "$h"`).
