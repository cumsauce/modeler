# `decal`

**Decals:** a decal projects a material onto whatever surface it covers
(graffiti, posters, stains, cracks, road markings, logos, bullet holes):
no geometry of its own, it follows curved and uneven surfaces. Make the
image as a material with `domain: "decal"` (a `tile`-meter image, top
row = the image's top, `opacity` 0 around the shape: like a leaf card),
then place it: `{"type": "decal", "name": "tag", "pbr": "graffiti",
"size": [1.2, 0.8], "on": "wall_back", "toward": "back", "at": [0.4,
1.5]}` sticks it on `wall_back`'s surface, travelling `toward` (down by
default; `at` is the two cross axes, as for `place`: down `[x, z]`,
back/front `[x, y]`, left/right `[z, y]`). Or give `translation` (the
box centre) and `direction` (the projection, default down) by hand.
`size` is the image's width and height in meters plus the box's depth
along the projection (default half the smaller side): everything within
it gets painted, so keep it shallow next to other parts. The image stays
upright on walls (its up = world up) and points back (-z) on floors;
`roll` turns it (degrees). `attach` makes it ride a part's moves; prefab
instances bring their decals. `kind` is for the game (`clustered`, the
default, or `forward`). The preview (window and agent views) draws every
decal as a Bevy `ForwardDecal` (clustered decals need bindless textures,
which Bevy turns off on Metal). The export writes each decal as a flat
textured quad on the surface with `extras.decal {size, kind}`, so any
viewer shows a sticker; `modeler_bevy`'s plugin swaps it for a
`ForwardDecal` (the camera needs a `DepthPrepass`) or, for `clustered`
where `ModelerDecals.clustered` allows, a `ClusteredDecal`. In a `.bsn`
map the map's own decals go into its ground glTF; prefab instances keep
theirs in their own file. The window lists decals in the Model and Map
panes (a Decals layer toggle in Map) and their materials under "Decal
materials" in Material mode. To strew many decals over a surface
(cracks, stains, leaves), use a `scatter` with `decal` instead of `input`
(see `scatter`).
