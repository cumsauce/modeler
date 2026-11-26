# `prefab`

- **Props with colliders**: a `prefab` item with `block: true` also makes a
  `block` marker `<name>_blk` (the game's `Block` component) filling the
  instance's world bounds, so a crate or a vending machine is solid in
  games that build colliders from markers. `catalog_describe` prints each
  entry's `bounds` (x / y / z extents in its own frame, size, centre; the
  origin is 0, 0, 0), so props are placed and sized without probing.
- **Prefabs as parts**: `{"type": "prefab", "name": "lamp1", "prefab": "lamp"}`
  instances a saved catalog model; its visible parts become `lamp1.pole`,
  `lamp1.shade`, ... and feed later ops like any part. The part list is
  captured when the item is applied; re-issue it after re-saving the prefab.
