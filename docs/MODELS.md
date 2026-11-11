# MODELS.md — procedural model catalog

Six models built procedurally through the modeler op graph, one server session
each (build → visual check → `save_prefab` → `.glb` export → this doc). Every
model below was screenshot-verified during the build (images go to `shots/`,
binary exports to `models/`; both are gitignored, so regenerate them locally). The existing `pink-flower` and `graph-flower`
catalog entries were left untouched.

Session note: this agent's MCP tool list was frozen without the
`mcp__modeler__*` tools, so the models were driven through the identical tool
surface by speaking MCP JSON-RPC to the `modeler --headless`/windowed binary
over stdio (`mcp_driver.py`, committed here). The op_* tools were confirmed
present in the server's `tools/list` before building.

Conventions used throughout (learned the hard way, see AGENTS.md):
- material ops run **before** array/mirror/transform ops that copy the part,
  so the copies inherit the color;
- boolean helper boxes are reused in-place as finished parts (glass, doors)
  so no stray geometry is left in the export;
- try-an-alternative flows use `jump`, which branches the history tree; the
  abandoned sibling subtree stays reachable.

---


## 1. Garden gate (`garden-gate`)

A wooden garden gate: eight pickets stepped out by a **linear array**
between two dark posts with caps and two stained rails.

**Branch story:** the first picket row used 6 pickets at 0.18 m spacing; after a
`jump` back to node #4 the array was re-issued with 8 pickets at 0.15 m. Both
versions live as sibling children of node #4 (`#5` and `#6`); the denser gate
was kept.

**Final part count:** 14

**Exact tool-call sequence:**

```text
1. `set_model_name` name=`garden-gate`
1. `op_primitive` name=`picket` kind=box size=[0.07, 0.9, 0.04] t=[0.0, 0.45, 0.0]
1. `op_primitive` name=`top_rail` kind=box size=[1.25, 0.07, 0.05] t=[0.6, 0.85, 0.0]
1. `op_primitive` name=`bottom_rail` kind=box size=[1.25, 0.07, 0.05] t=[0.6, 0.15, 0.0]
1. `op_material` name=`picket` <- `picket` color=[0.85, 0.75, 0.55, 1.0] roughness=0.8
1. `op_linear_array` name=`picket_row` 6 x `picket` offset=[0.18, 0.0, 0.0]
1. `history`
1. `jump` to node #4  **<- jump**
1. `op_linear_array` name=`picket_row` 8 x `picket` offset=[0.15, 0.0, 0.0]
1. `op_primitive` name=`post_l` kind=box size=[0.1, 1.25, 0.1] t=[-0.12, 0.625, 0.0]
1. `op_transform` name=`post_r` input=`post_l` translation=[1.32, 0.625, 0.0]
1. `op_transform` name=`post_cap_l` input=`post_l` translation=[-0.12, 1.33, 0.0] scale=[1.3, 0.1, 1.3]
1. `op_transform` name=`post_cap_r` input=`post_l` translation=[1.32, 1.33, 0.0] scale=[1.3, 0.1, 1.3]
1. `op_material` name=`top_rail` <- `top_rail` color=[0.55, 0.35, 0.2, 1.0] roughness=0.7
1. `op_material` name=`bottom_rail` <- `bottom_rail` color=[0.55, 0.35, 0.2, 1.0] roughness=0.7
1. `op_material` name=`post_l` <- `post_l` color=[0.45, 0.28, 0.16, 1.0] roughness=0.75
1. `op_material` name=`post_r` <- `post_r` color=[0.45, 0.28, 0.16, 1.0] roughness=0.75
1. `op_material` name=`post_cap_l` <- `post_cap_l` color=[0.35, 0.22, 0.12, 1.0] roughness=0.6
1. `op_material` name=`post_cap_r` <- `post_cap_r` color=[0.35, 0.22, 0.12, 1.0] roughness=0.6
1. `history`
1. `scene_summary`
1. `screenshot`
1. `save_prefab` description=`A wooden garden gate: eight tapered pickets fanned by a line...` tags=['gate', 'garden', 'wood', 'procedural', 'linear-array']
1. `export_model` /Users/lume/modeler/models/garden-gate.glb (glb)
```

**History tree (`history` at save time, `<-` = cursor):**

```text
#0 root, `name `garden-gate``, v0
#1 parent #0, `op primitive `picket` (box)`, v1
#2 parent #1, `op primitive `top_rail` (box)`, v2
#3 parent #2, `op primitive `bottom_rail` (box)`, v3
#4 parent #3, `op material `picket` <- `picket``, v4
#5 parent #4, `op linear `picket_row` = 6 x `picket``, v5
#6 parent #4, `op linear `picket_row` = 8 x `picket``, v5
#7 parent #6, `op primitive `post_l` (box)`, v6
#8 parent #7, `op transform `post_r` <- `post_l``, v7
#9 parent #8, `op transform `post_cap_l` <- `post_l``, v8
#10 parent #9, `op transform `post_cap_r` <- `post_l``, v9
#11 parent #10, `op material `top_rail` <- `top_rail``, v10
#12 parent #11, `op material `bottom_rail` <- `bottom_rail``, v11
#13 parent #12, `op material `post_l` <- `post_l``, v12
#14 parent #13, `op material `post_r` <- `post_r``, v13
#15 parent #14, `op material `post_cap_l` <- `post_cap_l``, v14
#16 parent #15, `op material `post_cap_r` <- `post_cap_r``, v15 <-
```

## 2. Cottage (`cottage`)

A cottage: cream box walls cut by **chained boolean differences**
(two through-windows and a door), a tent roof made from one wedge whose
z-scaled in-place transform becomes the west slope and whose z-**mirror**
becomes the east slope, chimney, and the boolean cutter boxes reused in place
as the glass panes and door fill (so intermediate shells `walls_1`/`walls_2`/
`body` are recolored to match the walls to avoid z-fighting rather than
deleted — the graph still derives `walls` from them).

**Branch story:** first built completely with a shallower roof
(scale z 0.5). Then `jump` to node #10 (the in-place roof transform) and
re-issuing it with scale z 0.65 branched the tree: nodes `#11`–`#24` are the
shallower-roof attempt, nodes `#25`–`#40` the steeper roof that was kept.

**Final part count:** 10

**Exact tool-call sequence:**

```text
1. `set_model_name` name=`cottage`
1. `op_primitive` name=`body` kind=box size=[2.4, 1.4, 1.8] t=[0.0, 0.7, 0.0]
1. `op_primitive` name=`win_cut_a` kind=box size=[0.5, 0.5, 2.0] t=[0.7, 0.9, 0.0]
1. `op_boolean` name=`walls_1` difference(`body`, `win_cut_a`)
1. `op_primitive` name=`win_cut_b` kind=box size=[0.5, 0.5, 2.0] t=[-0.7, 0.9, 0.0]
1. `op_boolean` name=`walls_2` difference(`walls_1`, `win_cut_b`)
1. `op_primitive` name=`door_cut` kind=box size=[0.5, 1.0, 2.0] t=[0.0, 0.5, 0.0]
1. `op_boolean` name=`walls` difference(`walls_2`, `door_cut`)
1. `op_primitive` name=`roof_h` kind=wedge size=[2.8, 0.8, 1.9] t=[0.0, 1.8, 0.0]
1. `op_transform` name=`roof_h` input=`roof_h` translation=[0.0, 1.8, -0.475] scale=[1.0, 1.0, 0.5]
1. `op_mirror` name=`roof_east` `roof_h` across z=0.0
1. `op_transform` name=`win_cut_a` input=`win_cut_a` scale=[1.08, 1.08, 1.0]
1. `op_transform` name=`win_cut_b` input=`win_cut_b` scale=[1.08, 1.08, 1.0]
1. `op_transform` name=`door_cut` input=`door_cut` scale=[1.1, 1.0, 1.0]
1. `op_primitive` name=`chimney` kind=box size=[0.35, 1.0, 0.35] t=[0.8, 1.9, 0.2]
1. `op_material` name=`walls` <- `walls` color=[0.92, 0.87, 0.72, 1.0] roughness=0.9
1. `op_material` name=`walls_1` <- `walls_1` color=[0.92, 0.87, 0.72, 1.0] roughness=0.9
1. `op_material` name=`walls_2` <- `walls_2` color=[0.92, 0.87, 0.72, 1.0] roughness=0.9
1. `op_material` name=`body` <- `body` color=[0.92, 0.87, 0.72, 1.0] roughness=0.9
1. `op_material` name=`roof_h` <- `roof_h` color=[0.62, 0.3, 0.2, 1.0] roughness=0.85
1. `op_material` name=`roof_east` <- `roof_east` color=[0.62, 0.3, 0.2, 1.0] roughness=0.85
1. `op_material` name=`win_cut_a` <- `win_cut_a` color=[0.6, 0.8, 0.9, 1.0] roughness=0.2 metalness=0.3
1. `op_material` name=`win_cut_b` <- `win_cut_b` color=[0.6, 0.8, 0.9, 1.0] roughness=0.2 metalness=0.3
1. `op_material` name=`door_cut` <- `door_cut` color=[0.35, 0.2, 0.12, 1.0] roughness=0.7
1. `op_material` name=`chimney` <- `chimney` color=[0.45, 0.42, 0.4, 1.0] roughness=0.95
1. `history`
1. `jump` to node #10  **<- jump**
1. `op_transform` name=`roof_h` input=`roof_h` translation=[0.0, 1.8, -0.6175] scale=[1.0, 1.0, 0.65]
1. `op_mirror` name=`roof_east` `roof_h` across z=0.0
1. `op_transform` name=`win_cut_a` input=`win_cut_a` scale=[1.08, 1.08, 1.0]
1. `op_transform` name=`win_cut_b` input=`win_cut_b` scale=[1.08, 1.08, 1.0]
1. `op_transform` name=`door_cut` input=`door_cut` scale=[1.1, 1.0, 1.0]
1. `op_primitive` name=`chimney` kind=box size=[0.35, 1.0, 0.35] t=[0.8, 1.9, 0.2]
1. `op_material` name=`walls` <- `walls` color=[0.92, 0.87, 0.72, 1.0] roughness=0.9
1. `op_material` name=`walls_1` <- `walls_1` color=[0.92, 0.87, 0.72, 1.0] roughness=0.9
1. `op_material` name=`walls_2` <- `walls_2` color=[0.92, 0.87, 0.72, 1.0] roughness=0.9
1. `op_material` name=`body` <- `body` color=[0.92, 0.87, 0.72, 1.0] roughness=0.9
1. `op_material` name=`roof_h` <- `roof_h` color=[0.62, 0.3, 0.2, 1.0] roughness=0.85
1. `op_material` name=`roof_east` <- `roof_east` color=[0.62, 0.3, 0.2, 1.0] roughness=0.85
1. `op_material` name=`win_cut_a` <- `win_cut_a` color=[0.6, 0.8, 0.9, 1.0] roughness=0.2 metalness=0.3
1. `op_material` name=`win_cut_b` <- `win_cut_b` color=[0.6, 0.8, 0.9, 1.0] roughness=0.2 metalness=0.3
1. `op_material` name=`door_cut` <- `door_cut` color=[0.35, 0.2, 0.12, 1.0] roughness=0.7
1. `op_material` name=`chimney` <- `chimney` color=[0.45, 0.42, 0.4, 1.0] roughness=0.95
1. `history`
1. `scene_summary`
1. `screenshot`
1. `save_prefab` description=`A cottage: cream box walls cut by chained boolean difference...` tags=['house', 'cottage', 'boolean', 'mirror', 'procedural']
1. `export_model` /Users/lume/modeler/models/cottage.glb (glb)
```

**History tree (`history` at save time, `<-` = cursor):**

```text
#0 root, `name `cottage``, v0
#1 parent #0, `op primitive `body` (box)`, v1
#2 parent #1, `op primitive `win_cut_a` (box)`, v2
#3 parent #2, `op boolean `walls_1` = difference(body, win_cut_a)`, v3
#4 parent #3, `op primitive `win_cut_b` (box)`, v4
#5 parent #4, `op boolean `walls_2` = difference(walls_1, win_cut_b)`, v5
#6 parent #5, `op primitive `door_cut` (box)`, v6
#7 parent #6, `op boolean `walls` = difference(walls_2, door_cut)`, v7
#8 parent #7, `op primitive `roof_h` (wedge)`, v8
#9 parent #8, `op transform `roof_h` <- `roof_h``, v9
#10 parent #9, `op mirror `roof_east` = `roof_h` across z`, v10
#11 parent #10, `op transform `win_cut_a` <- `win_cut_a``, v11
#12 parent #11, `op transform `win_cut_b` <- `win_cut_b``, v12
#13 parent #12, `op transform `door_cut` <- `door_cut``, v13
#14 parent #13, `op primitive `chimney` (box)`, v14
#15 parent #14, `op material `walls` <- `walls``, v15
#16 parent #15, `op material `walls_1` <- `walls_1``, v16
#17 parent #16, `op material `walls_2` <- `walls_2``, v17
#18 parent #17, `op material `body` <- `body``, v18
#19 parent #18, `op material `roof_h` <- `roof_h``, v19
#20 parent #19, `op material `roof_east` <- `roof_east``, v20
#21 parent #20, `op material `win_cut_a` <- `win_cut_a``, v21
#22 parent #21, `op material `win_cut_b` <- `win_cut_b``, v22
#23 parent #22, `op material `door_cut` <- `door_cut``, v23
#24 parent #23, `op material `chimney` <- `chimney``, v24
#25 parent #10, `op transform `roof_h` <- `roof_h``, v11
#26 parent #25, `op mirror `roof_east` = `roof_h` across z`, v12
#27 parent #26, `op transform `win_cut_a` <- `win_cut_a``, v13
#28 parent #27, `op transform `win_cut_b` <- `win_cut_b``, v14
#29 parent #28, `op transform `door_cut` <- `door_cut``, v15
#30 parent #29, `op primitive `chimney` (box)`, v16
#31 parent #30, `op material `walls` <- `walls``, v17
#32 parent #31, `op material `walls_1` <- `walls_1``, v18
#33 parent #32, `op material `walls_2` <- `walls_2``, v19
#34 parent #33, `op material `body` <- `body``, v20
#35 parent #34, `op material `roof_h` <- `roof_h``, v21
#36 parent #35, `op material `roof_east` <- `roof_east``, v22
#37 parent #36, `op material `win_cut_a` <- `win_cut_a``, v23
#38 parent #37, `op material `win_cut_b` <- `win_cut_b``, v24
#39 parent #38, `op material `door_cut` <- `door_cut``, v25
#40 parent #39, `op material `chimney` <- `chimney``, v26 <-
```

## 3. Well (`well`)

A stone well: cylinder base, **washer** rim over dark water, six
wooden posts ringed by a **radial array** (5 copies at `start_degrees: 60` so
the source post fills the 0° slot without duplication), a turned crossbar
(cylinder rotated 90° about z in place), rope and bucket, cone roof.

**Branch story:** the roof was first built at r=0.95; after `jump` to node #7
it was re-issued as r=1.1/h=0.45 (branch nodes `#19`–`#24`), which was kept.
The jump point sat before the crossbar/rope/bucket ops, so those were
re-issued in the branch too — jumping back to a snapshot restores that
snapshot's op list exactly.

**Final part count:** 13

**Exact tool-call sequence:**

```text
1. `set_model_name` name=`well`
1. `op_primitive` name=`base` kind=cylinder size=[0.55, 0.5, 0.0] t=[0.0, 0.25, 0.0]
1. `op_primitive` name=`rim` kind=washer size=[0.42, 0.55, 0.0] t=[0.0, 0.5, 0.0]
1. `op_primitive` name=`water` kind=cylinder size=[0.42, 0.06, 0.0] t=[0.0, 0.45, 0.0]
1. `op_primitive` name=`post` kind=cylinder size=[0.05, 0.8, 0.0] t=[0.62, 0.9, 0.0]
1. `op_material` name=`post` <- `post` color=[0.5, 0.32, 0.18, 1.0] roughness=0.8
1. `op_radial_array` name=`posts` 5 x `post` center=[0.0, 0.9, 0.0] degrees=360 start=60.0
1. `op_primitive` name=`roof` kind=cone size=[0.95, 0.35, 0.0] t=[0.0, 1.5, 0.0]
1. `op_primitive` name=`crossbar` kind=cylinder size=[0.05, 1.5, 0.0] t=[0.0, 1.25, 0.0]
1. `op_transform` name=`crossbar` input=`crossbar` rotation=[0.0, 0.0, 0.70710678, 0.70710678]
1. `op_primitive` name=`rope` kind=cylinder size=[0.02, 0.45, 0.0] t=[0.0, 1.02, 0.0]
1. `op_primitive` name=`bucket` kind=cylinder size=[0.12, 0.15, 0.0] t=[0.0, 0.72, 0.0]
1. `op_material` name=`base` <- `base` color=[0.58, 0.57, 0.6, 1.0] roughness=0.95
1. `op_material` name=`rim` <- `rim` color=[0.45, 0.44, 0.47, 1.0] roughness=0.9
1. `op_material` name=`water` <- `water` color=[0.1, 0.3, 0.5, 1.0] roughness=0.1
1. `op_material` name=`roof` <- `roof` color=[0.62, 0.3, 0.2, 1.0] roughness=0.85
1. `op_material` name=`crossbar` <- `crossbar` color=[0.55, 0.36, 0.2, 1.0] roughness=0.8
1. `op_material` name=`rope` <- `rope` color=[0.4, 0.35, 0.25, 1.0] roughness=0.9
1. `op_material` name=`bucket` <- `bucket` color=[0.45, 0.28, 0.15, 1.0] roughness=0.85
1. `history`
1. `jump` to node #7  **<- jump**
1. `op_primitive` name=`roof` kind=cone size=[1.1, 0.45, 0.0] t=[0.0, 1.55, 0.0]
1. `op_primitive` name=`crossbar` kind=cylinder size=[0.05, 1.5, 0.0] t=[0.0, 1.25, 0.0]
1. `op_transform` name=`crossbar` input=`crossbar` rotation=[0.0, 0.0, 0.70710678, 0.70710678]
1. `op_primitive` name=`rope` kind=cylinder size=[0.02, 0.45, 0.0] t=[0.0, 1.02, 0.0]
1. `op_primitive` name=`bucket` kind=cylinder size=[0.12, 0.15, 0.0] t=[0.0, 0.72, 0.0]
1. `op_material` name=`base` <- `base` color=[0.58, 0.57, 0.6, 1.0] roughness=0.95
1. `op_material` name=`rim` <- `rim` color=[0.45, 0.44, 0.47, 1.0] roughness=0.9
1. `op_material` name=`water` <- `water` color=[0.1, 0.3, 0.5, 1.0] roughness=0.1
1. `op_material` name=`roof` <- `roof` color=[0.55, 0.25, 0.18, 1.0] roughness=0.85
1. `op_material` name=`crossbar` <- `crossbar` color=[0.55, 0.36, 0.2, 1.0] roughness=0.8
1. `op_material` name=`rope` <- `rope` color=[0.4, 0.35, 0.25, 1.0] roughness=0.9
1. `op_material` name=`bucket` <- `bucket` color=[0.45, 0.28, 0.15, 1.0] roughness=0.85
1. `history`
1. `scene_summary`
1. `screenshot`
1. `save_prefab` description=`A stone well: cylinder base, washer rim over dark water, six...` tags=['well', 'garden', 'radial-array', 'washer', 'procedural']
1. `export_model` /Users/lume/modeler/models/well.glb (glb)
```

**History tree (`history` at save time, `<-` = cursor):**

```text
#0 root, `name `well``, v0
#1 parent #0, `op primitive `base` (cylinder)`, v1
#2 parent #1, `op primitive `rim` (washer)`, v2
#3 parent #2, `op primitive `water` (cylinder)`, v3
#4 parent #3, `op primitive `post` (cylinder)`, v4
#5 parent #4, `op material `post` <- `post``, v5
#6 parent #5, `op radial `posts` = 5 x `post` over 360°`, v6
#7 parent #6, `op primitive `roof` (cone)`, v7
#8 parent #7, `op primitive `crossbar` (cylinder)`, v8
#9 parent #8, `op transform `crossbar` <- `crossbar``, v9
#10 parent #9, `op primitive `rope` (cylinder)`, v10
#11 parent #10, `op primitive `bucket` (cylinder)`, v11
#12 parent #11, `op material `base` <- `base``, v12
#13 parent #12, `op material `rim` <- `rim``, v13
#14 parent #13, `op material `water` <- `water``, v14
#15 parent #14, `op material `roof` <- `roof``, v15
#16 parent #15, `op material `crossbar` <- `crossbar``, v16
#17 parent #16, `op material `rope` <- `rope``, v17
#18 parent #17, `op material `bucket` <- `bucket``, v18
#19 parent #7, `op primitive `roof` (cone)`, v8
#20 parent #19, `op primitive `crossbar` (cylinder)`, v9
#21 parent #20, `op transform `crossbar` <- `crossbar``, v10
#22 parent #21, `op primitive `rope` (cylinder)`, v11
#23 parent #22, `op primitive `bucket` (cylinder)`, v12
#24 parent #23, `op material `base` <- `base``, v13
#25 parent #24, `op material `rim` <- `rim``, v14
#26 parent #25, `op material `water` <- `water``, v15
#27 parent #26, `op material `roof` <- `roof``, v16
#28 parent #27, `op material `crossbar` <- `crossbar``, v17
#29 parent #28, `op material `rope` <- `rope``, v18
#30 parent #29, `op material `bucket` <- `bucket``, v19 <-
```

## 4. Robot (`robot`)

A boxy robot: torso/hips/head stack, cylinder legs and box feet
(second copies via transforms), one **capsule arm mirrored across x=0**,
cylinder eyes rotated to face forward then mirrored, antenna with knob.

**Branch story:** the arm was first a box (`#7`–`#24` chain); a `jump` to node
#12 re-issued it as a capsule (branch `#25`–`#36`), and the capsule was kept.
An earlier lesson is baked into the ordering here: mirrors/transforms that run
*before* their source's material op clone the uncolored source — the first
robot render had a white left arm — so every material now precedes its
mirror.

**Final part count:** 13

**Exact tool-call sequence:**

```text
1. `set_model_name` name=`robot`
1. `op_primitive` name=`torso` kind=box size=[0.7, 0.9, 0.4] t=[0.0, 1.05, 0.0]
1. `op_material` name=`torso` <- `torso` color=[0.35, 0.5, 0.65, 1.0] roughness=0.4 metalness=0.6
1. `op_primitive` name=`hips` kind=box size=[0.5, 0.25, 0.35] t=[0.0, 0.5, 0.0]
1. `op_material` name=`hips` <- `hips` color=[0.25, 0.3, 0.35, 1.0] roughness=0.5 metalness=0.5
1. `op_primitive` name=`leg` kind=cylinder size=[0.09, 0.4, 0.0] t=[0.15, 0.2, 0.0]
1. `op_material` name=`leg` <- `leg` color=[0.3, 0.3, 0.33, 1.0] roughness=0.4 metalness=0.7
1. `op_transform` name=`leg_r` input=`leg` translation=[-0.15, 0.2, 0.0]
1. `op_primitive` name=`foot` kind=box size=[0.18, 0.08, 0.28] t=[0.15, 0.04, 0.05]
1. `op_material` name=`foot` <- `foot` color=[0.2, 0.2, 0.22, 1.0] roughness=0.5 metalness=0.6
1. `op_transform` name=`foot_r` input=`foot` translation=[-0.15, 0.04, 0.05]
1. `op_primitive` name=`arm` kind=box size=[0.15, 0.55, 0.15] t=[0.42, 1.15, 0.0]
1. `op_material` name=`arm` <- `arm` color=[0.9, 0.5, 0.15, 1.0] roughness=0.45 metalness=0.4
1. `op_mirror` name=`arm_r` `arm` across x=0.0
1. `op_primitive` name=`head` kind=box size=[0.4, 0.35, 0.35] t=[0.0, 1.72, 0.0]
1. `op_material` name=`head` <- `head` color=[0.45, 0.6, 0.75, 1.0] roughness=0.4 metalness=0.5
1. `op_primitive` name=`eye` kind=cylinder size=[0.05, 0.03, 0.0] t=[0.1, 1.76, 0.175]
1. `op_material` name=`eye` <- `eye` color=[1.0, 0.25, 0.1, 1.0] roughness=0.15
1. `op_transform` name=`eye` input=`eye` rotation=[0.70710678, 0.0, 0.0, 0.70710678]
1. `op_mirror` name=`eye_r` `eye` across x=0.0
1. `op_primitive` name=`antenna` kind=cylinder size=[0.02, 0.18, 0.0] t=[0.0, 1.98, 0.0]
1. `op_material` name=`antenna` <- `antenna` color=[0.3, 0.3, 0.33, 1.0] roughness=0.4 metalness=0.7
1. `op_primitive` name=`knob` kind=sphere size=[0.05, 0.0, 0.0] t=[0.0, 2.1, 0.0]
1. `op_material` name=`knob` <- `knob` color=[1.0, 0.8, 0.2, 1.0] roughness=0.3
1. `history`
1. `jump` to node #12  **<- jump**
1. `op_primitive` name=`arm` kind=capsule size=[0.08, 0.55, 0.0] t=[0.42, 1.15, 0.0]
1. `op_material` name=`arm` <- `arm` color=[0.9, 0.5, 0.15, 1.0] roughness=0.45 metalness=0.4
1. `op_mirror` name=`arm_r` `arm` across x=0.0
1. `op_primitive` name=`head` kind=box size=[0.4, 0.35, 0.35] t=[0.0, 1.72, 0.0]
1. `op_material` name=`head` <- `head` color=[0.45, 0.6, 0.75, 1.0] roughness=0.4 metalness=0.5
1. `op_primitive` name=`eye` kind=cylinder size=[0.05, 0.03, 0.0] t=[0.1, 1.76, 0.175]
1. `op_material` name=`eye` <- `eye` color=[1.0, 0.25, 0.1, 1.0] roughness=0.15
1. `op_transform` name=`eye` input=`eye` rotation=[0.70710678, 0.0, 0.0, 0.70710678]
1. `op_mirror` name=`eye_r` `eye` across x=0.0
1. `op_primitive` name=`antenna` kind=cylinder size=[0.02, 0.18, 0.0] t=[0.0, 1.98, 0.0]
1. `op_material` name=`antenna` <- `antenna` color=[0.3, 0.3, 0.33, 1.0] roughness=0.4 metalness=0.7
1. `op_primitive` name=`knob` kind=sphere size=[0.05, 0.0, 0.0] t=[0.0, 2.1, 0.0]
1. `op_material` name=`knob` <- `knob` color=[1.0, 0.8, 0.2, 1.0] roughness=0.3
1. `history`
1. `scene_summary`
1. `screenshot`
1. `save_prefab` description=`A boxy robot built as a primitive stack: metallic blue torso...` tags=['robot', 'character', 'mirror', 'procedural']
1. `export_model` /Users/lume/modeler/models/robot.glb (glb)
```

**History tree (`history` at save time, `<-` = cursor):**

```text
#0 root, `name `robot``, v0
#1 parent #0, `op primitive `torso` (box)`, v1
#2 parent #1, `op material `torso` <- `torso``, v2
#3 parent #2, `op primitive `hips` (box)`, v3
#4 parent #3, `op material `hips` <- `hips``, v4
#5 parent #4, `op primitive `leg` (cylinder)`, v5
#6 parent #5, `op material `leg` <- `leg``, v6
#7 parent #6, `op transform `leg_r` <- `leg``, v7
#8 parent #7, `op primitive `foot` (box)`, v8
#9 parent #8, `op material `foot` <- `foot``, v9
#10 parent #9, `op transform `foot_r` <- `foot``, v10
#11 parent #10, `op primitive `arm` (box)`, v11
#12 parent #11, `op material `arm` <- `arm``, v12
#13 parent #12, `op mirror `arm_r` = `arm` across x`, v13
#14 parent #13, `op primitive `head` (box)`, v14
#15 parent #14, `op material `head` <- `head``, v15
#16 parent #15, `op primitive `eye` (cylinder)`, v16
#17 parent #16, `op material `eye` <- `eye``, v17
#18 parent #17, `op transform `eye` <- `eye``, v18
#19 parent #18, `op mirror `eye_r` = `eye` across x`, v19
#20 parent #19, `op primitive `antenna` (cylinder)`, v20
#21 parent #20, `op material `antenna` <- `antenna``, v21
#22 parent #21, `op primitive `knob` (sphere)`, v22
#23 parent #22, `op material `knob` <- `knob``, v23
#24 parent #12, `op primitive `arm` (capsule)`, v13
#25 parent #24, `op material `arm` <- `arm``, v14
#26 parent #25, `op mirror `arm_r` = `arm` across x`, v15
#27 parent #26, `op primitive `head` (box)`, v16
#28 parent #27, `op material `head` <- `head``, v17
#29 parent #28, `op primitive `eye` (cylinder)`, v18
#30 parent #29, `op material `eye` <- `eye``, v19
#31 parent #30, `op transform `eye` <- `eye``, v20
#32 parent #31, `op mirror `eye_r` = `eye` across x`, v21
#33 parent #32, `op primitive `antenna` (cylinder)`, v22
#34 parent #33, `op material `antenna` <- `antenna``, v23
#35 parent #34, `op primitive `knob` (sphere)`, v24
#36 parent #35, `op material `knob` <- `knob``, v25 <-
```

## 5. Pine tree (`pine-tree`)

A stylized pine: cylinder trunk plus three stacked cone canopy
tiers in graduated greens.

**Branch story:** after building the full tree, `jump` to node #4 and a
smaller top tier (r 0.3) was tried (branch `#9`–`#10`), then the original
fuller tier was **re-adopted by jumping back** to node #8 — the saved prefab
and export are the full-canopy version, with both attempts in the tree.

**Final part count:** 4

**Exact tool-call sequence:**

```text
1. `set_model_name` name=`pine-tree`
1. `op_primitive` name=`trunk` kind=cylinder size=[0.12, 0.9, 0.0] t=[0.0, 0.45, 0.0]
1. `op_primitive` name=`tier1` kind=cone size=[0.8, 0.8, 0.0] t=[0.0, 1.0, 0.0]
1. `op_primitive` name=`tier2` kind=cone size=[0.62, 0.7, 0.0] t=[0.0, 1.55, 0.0]
1. `op_primitive` name=`tier3` kind=cone size=[0.45, 0.6, 0.0] t=[0.0, 2.05, 0.0]
1. `op_material` name=`trunk` <- `trunk` color=[0.4, 0.26, 0.15, 1.0] roughness=0.9
1. `op_material` name=`tier1` <- `tier1` color=[0.15, 0.45, 0.2, 1.0] roughness=0.85
1. `op_material` name=`tier2` <- `tier2` color=[0.17, 0.5, 0.22, 1.0] roughness=0.85
1. `op_material` name=`tier3` <- `tier3` color=[0.19, 0.55, 0.25, 1.0] roughness=0.85
1. `history`
1. `jump` to node #4  **<- jump**
1. `op_primitive` name=`tier3` kind=cone size=[0.3, 0.45, 0.0] t=[0.0, 2.0, 0.0]
1. `op_material` name=`tier3` <- `tier3` color=[0.19, 0.55, 0.25, 1.0] roughness=0.85
1. `history`
1. `jump` to node #8  **<- jump**
1. `scene_summary`
1. `screenshot`
1. `save_prefab` description=`A stylized pine tree: brown cylinder trunk with three stacke...` tags=['tree', 'pine', 'nature', 'cone', 'procedural']
1. `export_model` /Users/lume/modeler/models/pine-tree.glb (glb)
```

**History tree (`history` at save time, `<-` = cursor):**

```text
#0 root, `name `pine-tree``, v0
#1 parent #0, `op primitive `trunk` (cylinder)`, v1
#2 parent #1, `op primitive `tier1` (cone)`, v2
#3 parent #2, `op primitive `tier2` (cone)`, v3
#4 parent #3, `op primitive `tier3` (cone)`, v4
#5 parent #4, `op material `trunk` <- `trunk``, v5
#6 parent #5, `op material `tier1` <- `tier1``, v6
#7 parent #6, `op material `tier2` <- `tier2``, v7
#8 parent #7, `op material `tier3` <- `tier3``, v8
#9 parent #4, `op primitive `tier3` (cone)`, v5
#10 parent #9, `op material `tier3` <- `tier3``, v6 <-
```

## 6. Lighthouse (`lighthouse`)

A lighthouse: stone base, white tower with a **boolean-cut
doorway** (the cut box reused in place as the brown door fill), three red
stripe bands, washer gallery ringed by 8 railing posts from a **radial
array**, glowing lantern, red cone cap.

**Branch story (undo/redo this time):** after the finished model at node #23,
the cap was deliberately upserted floating at y=3.8 (node `#24`), `undo`
returned to #23, `redo` stepped forward to #24 again to compare, and `jump`
to #23 restored the correct model for saving/export.

**Final part count:** 18

**Exact tool-call sequence:**

```text
1. `set_model_name` name=`lighthouse`
1. `op_primitive` name=`base` kind=cylinder size=[1.0, 0.3, 0.0] t=[0.0, 0.15, 0.0]
1. `op_material` name=`base` <- `base` color=[0.5, 0.5, 0.52, 1.0] roughness=0.95
1. `op_primitive` name=`tower` kind=cylinder size=[0.55, 2.2, 0.0] t=[0.0, 1.4, 0.0]
1. `op_primitive` name=`door_cut` kind=box size=[0.3, 0.5, 0.8] t=[0.0, 0.35, 0.5]
1. `op_boolean` name=`tower_hollow` difference(`tower`, `door_cut`)
1. `op_transform` name=`door_cut` input=`door_cut` translation=[0.0, 0.35, 0.52] scale=[1.1, 1.05, 0.06]
1. `op_primitive` name=`stripe1` kind=cylinder size=[0.56, 0.3, 0.0] t=[0.0, 0.8, 0.0]
1. `op_primitive` name=`stripe2` kind=cylinder size=[0.56, 0.3, 0.0] t=[0.0, 1.4, 0.0]
1. `op_primitive` name=`stripe3` kind=cylinder size=[0.56, 0.3, 0.0] t=[0.0, 2.0, 0.0]
1. `op_primitive` name=`gallery` kind=washer size=[0.55, 0.75, 0.0] t=[0.0, 2.55, 0.0]
1. `op_primitive` name=`rail_post` kind=cylinder size=[0.02, 0.25, 0.0] t=[0.68, 2.7, 0.0]
1. `op_material` name=`rail_post` <- `rail_post` color=[0.15, 0.15, 0.18, 1.0] roughness=0.6
1. `op_radial_array` name=`rails` 7 x `rail_post` center=[0.0, 2.7, 0.0] degrees=360 start=45.0
1. `op_primitive` name=`lantern` kind=cylinder size=[0.4, 0.45, 0.0] t=[0.0, 2.9, 0.0]
1. `op_primitive` name=`cap` kind=cone size=[0.5, 0.3, 0.0] t=[0.0, 3.28, 0.0]
1. `op_material` name=`tower_hollow` <- `tower_hollow` color=[0.93, 0.92, 0.88, 1.0] roughness=0.85
1. `op_material` name=`door_cut` <- `door_cut` color=[0.35, 0.22, 0.12, 1.0] roughness=0.75
1. `op_material` name=`stripe1` <- `stripe1` color=[0.8, 0.2, 0.15, 1.0] roughness=0.8
1. `op_material` name=`stripe2` <- `stripe2` color=[0.8, 0.2, 0.15, 1.0] roughness=0.8
1. `op_material` name=`stripe3` <- `stripe3` color=[0.8, 0.2, 0.15, 1.0] roughness=0.8
1. `op_material` name=`gallery` <- `gallery` color=[0.2, 0.2, 0.24, 1.0] roughness=0.7
1. `op_material` name=`lantern` <- `lantern` color=[1.0, 0.9, 0.55, 1.0] roughness=0.15
1. `op_material` name=`cap` <- `cap` color=[0.7, 0.18, 0.14, 1.0] roughness=0.8
1. `history`
1. `op_primitive` name=`cap` kind=cone size=[0.5, 0.3, 0.0] t=[0.0, 3.8, 0.0]
1. `undo` undo  **<- undo**
1. `redo` redo  **<- redo**
1. `jump` to node #23  **<- jump**
1. `history`
1. `scene_summary`
1. `screenshot`
1. `save_prefab` description=`A lighthouse: stone base, white cylinder tower with a boolea...` tags=['lighthouse', 'nautical', 'boolean', 'radial-array', 'procedural']
1. `export_model` /Users/lume/modeler/models/lighthouse.glb (glb)
```

**History tree (`history` at save time, `<-` = cursor):**

```text
#0 root, `name `lighthouse``, v0
#1 parent #0, `op primitive `base` (cylinder)`, v1
#2 parent #1, `op material `base` <- `base``, v2
#3 parent #2, `op primitive `tower` (cylinder)`, v3
#4 parent #3, `op primitive `door_cut` (box)`, v4
#5 parent #4, `op boolean `tower_hollow` = difference(tower, door_cut)`, v5
#6 parent #5, `op transform `door_cut` <- `door_cut``, v6
#7 parent #6, `op primitive `stripe1` (cylinder)`, v7
#8 parent #7, `op primitive `stripe2` (cylinder)`, v8
#9 parent #8, `op primitive `stripe3` (cylinder)`, v9
#10 parent #9, `op primitive `gallery` (washer)`, v10
#11 parent #10, `op primitive `rail_post` (cylinder)`, v11
#12 parent #11, `op material `rail_post` <- `rail_post``, v12
#13 parent #12, `op radial `rails` = 7 x `rail_post` over 360°`, v13
#14 parent #13, `op primitive `lantern` (cylinder)`, v14
#15 parent #14, `op primitive `cap` (cone)`, v15
#16 parent #15, `op material `tower_hollow` <- `tower_hollow``, v16
#17 parent #16, `op material `door_cut` <- `door_cut``, v17
#18 parent #17, `op material `stripe1` <- `stripe1``, v18
#19 parent #18, `op material `stripe2` <- `stripe2``, v19
#20 parent #19, `op material `stripe3` <- `stripe3``, v20
#21 parent #20, `op material `gallery` <- `gallery``, v21
#22 parent #21, `op material `lantern` <- `lantern``, v22
#23 parent #22, `op material `cap` <- `cap``, v23 <-
#24 parent #23, `op primitive `cap` (cone)`, v24
```

## Summary

| Model | Ops used | Parts (final scene) | History nodes | Complexity |
|---|---|---|---|---|
| Garden gate | primitive, material, linear_array, transform (+ jump branch) | 14 | 17 | low |
| Cottage | primitive, boolean ×3, material, transform, mirror (+ jump branch) | 10 | 41 | high |
| Well | primitive (cylinder/washer/cone), material, radial_array, transform (+ jump branch) | 13 | 31 | medium |
| Robot | primitive (box/cylinder/capsule/sphere), material, transform, mirror (+ jump branch) | 13 | 37 | medium |
| Pine tree | primitive (cylinder/cone), material (+ jump-try-jump-back) | 4 | 11 | low |
| Lighthouse | primitive, boolean, transform, material, radial_array (+ undo/redo/jump) | 18 | 25 | high |
