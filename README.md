# modeler

A mesh modeling tool driven by an MCP server, with a live Bevy preview
window. Built standalone out of the friendSLOP workspace; no game code.

## Units

**1 unit = 1 meter**, everywhere: op sizes and positions, bounds, the
preview, saved prefabs and glTF exports (also meters, so they import at
true scale). Y is up and the ground is y = 0. Model at real-world size.

## Run

```sh
# Booleans use Manifold (C++): the first build needs git, cmake and a C++
# compiler, and clones the library from the network.
cargo run                # MCP server on stdio + preview window
cargo run -- --headless  # no window/render, for CI or remote agents
```

Point your agent's MCP config at the `modeler` binary
(`cargo run` or `target/debug/modeler`).

## What's inside

- **Primitives**: box, sphere, cylinder, cone, torus, capsule, plane,
  wedge, disc, washer; raw triangle meshes.
- **Op graph**: an ordered list of steps (primitive, extrude, revolve, sweep, hull, round, group, transform, boolean,
  radial/linear array, mirror, material) that derive the parts. Ops are
  upserted by output name, so re-issuing one is a parameter tweak, and
  the whole model re-derives. `mirror` bakes the reflection so it
  renders correctly.
- **History tree**: every edit snapshots the model into a tree with a
  cursor — undo/redo, jump anywhere, and editing after a jump branches
  instead of discarding.
- **Prefab catalog**: non-destructive saves (op graph + history tree +
  annotations; procedural geometry re-derives on load; atomic writes).
  Catalog lives in `~/Library/Application Support/friendslop/modeler/prefabs`
  (carried over from the friendSLOP days).
- **Annotations**: comments pinned to a part/face, typed in the preview
  window or via tools.

## Preview window controls

Mouse drag orbits / pans, scroll zooms, `G` toggles the graph + history
panel, click a part then type to annotate (Enter commits, Esc cancels).
