# `light`

**Lights:** `{"type": "light", "name": "bulb", "kind": "point",
"translation": [0, 1.4, 0], "color": "#ffd8a8", "intensity": 800,
"attach": "lamp_shade"}`. Units are Bevy's: lumens for point/spot, lux for
directional (defaults 2000 / 3000); at Bevy's default camera exposure
(EV100 9.7, daylight) indoor lights need tens of thousands of lumens to
read, so a game lighting an interior lowers its exposure instead. `direction`
aims spot/directional lights (default down); spot cones take
`outer_degrees` (35) and `inner_degrees` (0.8 x outer). `attach` makes the
light ride its part through moves and places; prefab instances bring their
lights (`inst.bulb`). Colours take style tokens (`"@glow"`). Lights export
as KHR_lights_punctual (Bevy's loader spawns them; `shadows` goes in node
extras since glTF has no flag). The preview lights them on top of the
studio; `agent_views(..., lighting: "scene")` renders with the model's
lights only (studio off, faint environment) to judge how the level is lit.
