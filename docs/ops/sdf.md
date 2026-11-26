# `sdf`

- **`sdf`** models organic solids as signed distance fields: analytic
  shapes combined by smooth booleans, warped as a field, meshed once
  (Manifold's level set) into one closed solid. Cleaner and faster than
  `blob` (no meshes to sample) and the way to build creatures, mushrooms,
  bones, teeth, sculpted forms. `shapes` run in order, each joining the
  field before it by its `op` (`union` default, `subtract`, `intersect`)
  with smooth radius `blend` (per shape, else the item's, default a fifth of
  the thinnest shape; 0 = sharp):
  - `sphere` {radius, at}; `ellipsoid` {size: radii [x, y, z], at,
    rotation}; `box` {size: full [x, y, z], round, at, rotation};
    `capsule` {from, to, radius: r or [r_from, r_to]} (a rounded cone:
    limbs, horns, fingers); `cylinder` {radius, height, round, at,
    rotation}; `torus` {size: [ring, tube], at, rotation};
  - `chain` {points: [[x, y, z, r], ...], smooth} (a tube whose radius
    follows the points, a Catmull-Rom through them by default);
  - `part` {part}: a closed solid of the model sampled as a field (this is
    what `blob` does, mixable with analytic shapes);
  - `group` {shapes}: combined on their own first, then joined as one (to
    subtract a blended cluster).
  `mirror: "x"` folds the field so you model only the +x side of a
  symmetric creature (shapes crossing x = 0 should be symmetric
  themselves; `"z"`, `"xz"` for four-fold). `warps` act on the whole field
  in order, with the deform fields: `twist` / `bend` / `taper` warp space
  over the field's box at that point of the list, `displace` adds noise to
  the surface (amount, scale, octaves, seed, noise). **Local sculpt
  warps** (sdf, skin and body `warps`) act only within `radius` meters of
  `at` (a point; an anchor: a skin / body joint name such as `"head"`,
  `"part:top"` or `"part:joint"` on an sdf, which then reads that part, or
  anchor words on the field's own box, `"top-front"`), fading by
  `falloff` (`smooth` default, `linear`, `sharp`), with zero effect
  outside: `inflate` (`amount` meters, + grows, - shrinks), `push`
  (`direction` + `amount` meters: a brow pushed forward), `pinch`
  (`amount` 0..1, space pulled toward `at`: a waist, a crease), `flatten`
  (`normal` + `amount` 0..1 toward the plane through `at`: a cheek, the
  sole of a foot), `smooth` (`amount` 0..1, a local blur of the field:
  softens a lump or a sharp blend; `size` is the blur reach, default
  `radius / 3`, and each stacked smooth costs one blur). They run in list
  order with the space warps. They never fold the surface: push, pinch
  and flatten are capped (a push moves at most about 0.53 x `radius` with
  the smooth falloff, 0.8 x linear, 0.4 x sharp), so for a bigger change
  use a larger `radius` or several warps. Under the item's `mirror` a
  local warp is symmetric by default (placed at x < 0 it acts on both
  sides); `"mirror": false` on the warp keeps it one-sided. A `body` has no
  field mirror, but its local warps are symmetric the same way: each is
  applied at `at` and at its x-reflection (direction / normal reflected),
  once if it is on the centre line (`"mirror": false` keeps it on one side),
  so `shoulder_l` inflates both shoulders and asymmetric joint overrides
  still work. An `at` of
  `"part:anchor"` works on sdf, skin and body and makes that part a
  dependency. A fuller chest:
  `{"type": "body", "name": "guy", "preset": "biped", "warps": [{"mode":
  "inflate", "at": "chest", "radius": 0.15, "amount": 0.015}]}`.
  `resolution` (voxel
  meters) defaults to 1/120 of the size, finer for thin shapes, at most
  about 12M samples; an explicit one may go to 27M. Evaluation is
  bucketed, so hundreds of shapes stay fast. A critter:
  `{"type": "sdf", "name": "critter", "mirror": "x", "blend": 0.03,
  "shapes": [{"shape": "ellipsoid", "size": [0.12, 0.1, 0.16], "at": [0,
  0.16, 0]}, {"shape": "sphere", "radius": 0.075, "at": [0, 0.27, 0.15]},
  {"shape": "capsule", "from": [0.07, 0.12, 0.09], "to": [0.09, 0, 0.11],
  "radius": [0.03, 0.022]}, {"shape": "sphere", "radius": 0.02, "at":
  [0.035, 0.29, 0.215], "op": "subtract", "blend": 0.008}]}` (body, head,
  legs, eye sockets on both sides).
