# `panel`

- **`panel`** recesses a grid of panels into one face of a closed solid
  (walls, hull plating, doors, crate sides, cabinet fronts): `{"type":
  "panel", "name": "door", "input": "door_raw", "face": "front", "rows": 2,
  "cols": 1, "inset": 0.06, "depth": 0.008, "gap": 0.05}`. `rows` run down
  the face (along y on the sides, z on top / bottom), `cols` across it;
  `inset` (alias `border`) is the frame left round the grid, `gap` (alias
  `seam`) the ribs between panels, all at the face. The face is the side of
  the input's box, so it suits flat faces flush with it. The result keeps
  the input's material (hide the input).
