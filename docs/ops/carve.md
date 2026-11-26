# `carve`

- **`carve`** models interiors by subtraction (Quake / Hammer style): a solid
  mass has rooms taken out of it, and the result is only the faces that look
  into the void, as **one-sided sheets** (`sheet`, normals into the rooms, so
  no hidden outer faces, no z-fighting, no wall thickness to fight with).
  `{"type": "carve", "name": "bunker", "cutters": ["hall", "cell", "door"],
  "split_by": "facing", "markers": true, "collider": "mesh"}`
  (`src/carve.rs`).
  - **Inputs**: `cutters` are closed parts (exact names, in priority order; a
    face inside two belongs to the first; hide them). `rooms: [{name,
    footprint: [[x, z], ...], height, base?}]` are rooms given as an outline
    (an L-shaped lobby is 6 points): extruded from `base` and made as hidden
    parts of that name, usable as cutters (`split_by`, markers). `solid` is
    the mass; **omit it** and a box around the cutters is made, `wall`
    meters (0.3) thicker on every side, so a level needs no mass at all.
  - **`slabs`** put floors inside the carved volume: `{"footprint":
    [[[x, z], ...], [hole ring]], "top": 4.4, "thickness": 0.3}` (more rings
    are holes: a stairwell, the open middle of a ring balcony). A slab is
    added to the carved mass, so its top reads as floor, its underside as
    ceiling and its edges as wall, with the same merging, splitting by
    cutter / facing, materials and `blocks` colliders as everything else: a
    mezzanine, a loft or a catwalk is one item, not a pile of boxes.
  - **Openings are cutters**: a doorway is a box through the wall that
    overlaps both rooms. Where cutters touch or overlap the shared wall is
    gone (the union is carved), so rooms connect by construction. A cutter
    that pokes out of the mass opens it to the outside.
  - **Outputs** (a part is skipped when empty): `split_by` "none" (default)
    `name`; "facing" `name_floor`, `name_wall`, `name_ceiling` (by normal:
    up = floor, down = ceiling); "cutter" `name_<cutter>`; "both"
    `name_<cutter>_<facing>`. Split by cutter, each sheet wears **its
    cutter's material** (a cutter with no material of its own takes the
    solid's), so give each cutter a material before carving: wood floor in
    the hall, steel in the cell. `material` items on the outputs
    (`"bunker_*_floor"`) work as for any part, and tile materials map at
    metric scale. `keep` "outer" / "both" adds `name_outer`, the mass's skin
    seen from outside (default "inner"). With auto-solid the mass reaches
    `wall` below the lowest cutter, which moves the ground level `grounded`
    uses: give `keep: outer` an explicit `solid` standing on y = 0.
  - **`merge`** (default true) joins coplanar triangles into whole polygons
    per wall / floor / ceiling (holes kept: a wall with a door hole is one
    polygon with a hole, not a triangle soup), so a tile material and AO
    atlas stay clean; `false` keeps Manifold's triangles.
  - **`markers: true`** also makes game markers from the cutters' boxes:
    `<name>_room_<cutter>` (kind `room`, box size, `data.cutter`) and
    `<name>_opening_<cutter>` (kind `opening`: a cutter touching two or more
    cutters that don't touch each other, or one that touches a single room
    and pokes outside; `data.connects` lists the rooms, `data.outside`).
    They export like `marker` items (and into a `.bsn` map) and follow the
    carve when its cutters move (cutter boxes are read after the graph).
  - **`collider: "mesh"`** gives every visible sheet a static trimesh
    collider (a rule like `collider` items; a later rule wins). Trimesh
    colliders are two-sided in physics engines, so the player can't get out
    of (or into) a room through a wall; there is no separate shell to build.
  - **`collider: "blocks"`** is for games that build colliders from markers
    (friendSLOP reads `Block` markers, not glTF collider extras): the carved
    mass (walls, floor, ceiling, lintels over doors) is cut into axis-aligned
    boxes and made as `block` markers `<name>_blk_<i>` with a `Block`
    component, merged into long runs (a plain room is a handful). Thickness
    is `wall` (set it to the thickness you want, or give a `solid`). Exact
    for axis-aligned rooms, steps on slanted faces; more than 400k grid
    cells (rooms with very many distinct edges) is an error pointing at
    `mesh`. They follow the carve when it changes, draw as magenta gizmos
    like any marker, and need the project's `components.json` to define
    `Block` for a `.bsn` export.
  - **Checks**: `verify walkable` rasterizes sheets like any surface (floors
    carry the agent, a doorway needs the agent's `height` of headroom: the
    default 1.8 m agent passes a 2.1 m door and fails a 1.5 m one), `validate` / `grounded` /
    `collisions` treat sheets as sheets. An error names the problem when
    nothing is carved (cutters miss the mass) or the cutters swallow it.
    Everything is Manifold's `solid - union(cutters)`; a face looks into the
    void when the point just in front of it lies inside a cutter, which also
    names its room.
  - It is a terminal step: the sheets are open, so they can't feed a
    boolean. Do the cutting first, then material, scatter, decals, lights.
