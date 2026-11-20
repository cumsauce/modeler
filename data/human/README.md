# human.bin: the `human` body template

The data file of the `body` preset `human` (see AGENTS.md, the `body`
bullet). One zstd-compressed little-endian blob; the layout is documented
in `src/human/asset.rs`.

## Source and license

- MakeHuman core assets, `makehuman/data/` of
  <https://github.com/makehumancommunity/makehuman>, commit `a8bc2d54`.
- The base mesh, morph targets, skeleton and weights are released under
  **CC0 1.0** (the repository's `LICENSE.md`; `default_weights.mhw` and
  `default.mhskel` carry `"license": "CC0"` in their headers). No
  attribution is required. MakeHuman's *source code* is AGPL; none of it is
  used here: only the data files are parsed.

## What is inside

Packed by `cargo run --example pack_human -- <repo>/makehuman/data`
(it only parses data; prints the breakdown below).

- Mesh (`3dobjs/base.obj`, decimeters converted to meters, y up, facing
  +z): the closed `body` skin (13,378 quads) and the helpers kept for
  extras: `helper-l-eye` / `helper-r-eye` (fit the eyeballs),
  `helper-upper-teeth`, `helper-lower-teeth`, `helper-tongue`; quads, uvs
  (15,066). 14,862 vertices kept (with the joint cubes' vertices).
- 326 joint refs from `rigs/default.mhskel` (vertex sets whose mean is a
  joint, so joints follow the morphs), its 163 bones (head / tail refs,
  parents) and `rigs/default_weights.mhw` (per bone, u16 weights).
- 1,280 targets (`targets/**.target`), sparse rows of `u16 vertex +
  i16 x 3` in 0.1 mm units (lossless: the files have 3 decimals in dm),
  rows on dropped vertices and all-zero rows removed.
- 363 detail keys named after `modifiers/*_modifiers.json` (pairs
  `{target}-{min|max}`; 83 keys for target files no modifier names, by
  file stem) and 34 expression units (one target per ethnicity).

| group | files | rows | ~compressed |
|---|---|---|---|
| macrodetails/height | 144 | 2,112,574 | 7.81 MB |
| macrodetails/proportions | 108 | 1,158,269 | 2.44 MB |
| macrodetails/ethnic | 24 | 356,688 | 1.38 MB |
| macrodetails/universal | 72 | 285,633 | 0.98 MB |
| armslegs | 140 | 127,141 | 0.30 MB |
| bodyshapes | 22 | 64,248 | 0.21 MB |
| breast (cup / firmness macro) | 216 | 78,379 | 0.14 MB |
| head | 27 | 80,896 | 0.11 MB |
| expression (units) | 102 | 34,917 | 0.10 MB |
| measure | 40 | 109,907 | 0.09 MB |
| other detail groups | 385 | ~200,000 | ~0.3 MB |
| mesh, uvs, joints, skeleton, weights | | | 0.64 MB |
| **file** | | | **14.46 MB** |

Dropped: the clothes and hair helpers (`helper-tights`, `helper-skirt`,
`helper-hair`), `helper-genital`, the eyelash cards, the joint cubes'
faces (their vertices stay, for the joint refs), the weights and target
rows on those vertices (the `genitals/` targets are therefore empty),
MakeHuman's textures, proxies, poses and expressions-as-poses (`.mhpose`).

## Where it must be

The loader looks at, in order: `$MODELER_HUMAN_ASSET`; beside the running
binary (`human.bin`, `data/human/human.bin`); this repository's
`data/human/human.bin`; `data/human/human.bin` under the working
directory. For the installed build in `bin/modeler/`, copy it to
`bin/modeler/human.bin`.
