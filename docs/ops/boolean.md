# `boolean`

- **Booleans** run on Manifold (the kernel OpenSCAD and Blender use): outputs
  are closed solids, coplanar and curved-on-curved cuts are exact, and input
  UVs ride through; normals are creased at 40°. **Both inputs must be closed
  solids**: disc/plane/washer (open sheets) error with "is not a closed
  solid". A failed op rolls back the whole call; the graph never stays broken.
