# Skeletons and animation

**Skeletons and animation:** Rig a model after its shape and materials are done: shape, then materials,
then the rig. Rig items are `apply_ops` items like any other (upserts,
params, atomic, undoable). They run after the graph, as `move` does, so the
geometry recipe stays as it is. One pose evaluator serves every consumer:
agent views, verify, inspect, export and the window.

- **Bone frames are world-aligned at rest by default.** A bone has a `head`
  (its pivot), a `tail` and a parent. By default every rest rotation is the
  identity, so a clip rotation `{"x": 25}` means "pitch 25 degrees about
  world x from rest", with no bone roll to think about. A bone may set a
  rest frame: `rest` (a world rotation in any rotation form) and/or `roll`
  (degrees about head -> tail, with the frame's +y along the bone; even
  `roll: 0` aligns the frame to the bone). That bone's clip channels are
  then in its own frame (`{"x": 25}` turns about its local x), and
  `mirror: "x"` twins get the mirrored frame, so mirrored tracks stay
  symmetric. Exports write the rest frame as the joint node's rotation
  (inverse binds to match). Clip tracks are offsets from rest, composed
  down the tree. Built skeletons and clips are cached by a content hash of
  the rig ops and the parts they resolve against (`anim::rig_key`), so a
  lookup per frame is cheap. Limit: the runtime sway plugin
  (`modeler_bevy`) still assumes world-aligned rests.
