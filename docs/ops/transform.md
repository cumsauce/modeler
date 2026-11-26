# `transform`

- **Transforms** are absolute by default; `relative: true` makes them offsets
  from the input (translation adds, rotation composes, scale multiplies).
  With `pivot`, a relative rotation/scale turns about that world point
  instead of the part's own origin.
- A transform whose output is its own input is an in-place edit: re-issuing
  it replaces it, so a relative transform does not apply twice.
- To move a set of parts as one rigid body after the graph (a door on its
  hinge, a lean), use `move` instead: it keeps the recipe readable and never
  feeds later ops.
