# Oval and superellipse sections, tris / bake

- **Oval sections**: every `chain` (sdf) and `skin` point may be
  `[x, y, z, [rx, ry], roll?]` instead of `[x, y, z, r]`: an ellipse
  across the tube, `rx` sideways and `ry` along the tube's up (world up
  on a level tube, so `[0.15, 0.1]` on a chain along z is 0.3 wide and
  0.2 tall; on a vertical tube `rx` is x and `ry` is z), turned `roll`
  degrees about the tangent. The frame is carried along the curve
  without twisting, and sections, radii and roll interpolate smoothly, so
  torsos, snouts, necks, tails, fins and fenders need not be round. A
  chain with any oval point samples finer (about 1 degree of bend).
- **Superellipse sections**: a sixth entry, `[x, y, z, [rx, ry], roll,
  n]`, makes the section a superellipse of exponent `n` (2 = the ellipse,
  3-5 a rounded box, 1..40): car bodies, boxy torsos, planks.
- **`tris` / `bake` on `sdf`, `skin`, `body`**: `"tris": 4000` meshes
  the field only as finely as a 4000-triangle result needs and
  decimates it in the same item (the dense mesh is never a part), so one
  item replaces the sdf + decimate + hide_part triple; `"bake": true`
  also bakes the dense surface into the light part's normal and height
  maps (as `decimate`'s bake, meshed at full detail).
