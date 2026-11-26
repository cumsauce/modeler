# `move`

- **`move`** is a parent transform (one level of hierarchy): every part
  matching `parts` moves as one rigid body, rotated about `pivot` (default:
  the centre of their bottom face, fixed when issued) and then translated.
  It applies after the whole graph is evaluated, so no op ever reads the
  moved result: mirrors, arrays and cuts are built from the unmoved parts and
  carried along, and the model's recipe stays readable. Several moves compose
  in list order (later = outer parent). Give it a `name` and re-issue that
  name to adjust, e.g. lean a bike onto its kickstand with
  `{"type": "move", "name": "lean", "parts": ["*"], "axis": [1, 0, 0], "degrees": 8, "pivot": [0, 0, 0]}`.
  Every move/place result says where the moved parts' centre went
  ("0.25 m front (+z)": a door swinging the wrong way shows here) and
  warns about parts **left behind** (they touched a moved part and now
  touch nothing: a handle not listed with its door) and **new overlaps**
  with unmoved parts (the door swinging into its cabinet).

- **Anchor pivots**: a move's `pivot` may be a point or an anchor on a
  bounding box: `"bottom-back"` (the moved parts' own box) or
  `"laptop_screen:bottom-front"` (another part's box; `*` patterns work).
  Words: left/right (x), bottom/top (y), back/front (z, front = +z),
  center; axes not named stay centred. Anchors and the default pivot are
  measured on the parts *without* this move, so re-issuing a named move
  never drifts. A laptop hinge: `{"type": "move", "name": "hinge", "parts":
  ["screen"], "axis": [1, 0, 0], "degrees": -15, "pivot": "screen:bottom-front"}`.
