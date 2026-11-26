# `sweep`

- **`sweep`** runs a closed profile along a 3D polyline (pipes, rails,
  handles, frames, hoses); bends are mitred to keep the thickness, and
  `closed: true` loops the path (a ring) instead of capping it. Profile u is
  along the frame normal (world up to start, or +z on a vertical path).
