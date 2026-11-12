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

Point your agent's MCP config at the proxy, so every agent session shares
one server and one preview window:

```json
"modeler": {"type": "stdio", "command": "python3",
            "args": ["/path/to/modeler/mcp_driver.py", "--proxy"]}
```

The first session starts the shared daemon (windowed); later sessions join
it. Pointing the config at `target/debug/modeler` instead gives each session
its own server and window.

## In CI (Linux, headless)

Every push to `main` builds a Linux x86_64 binary (no window, no GPU, no
audio) and publishes it as the rolling
[`latest`](https://github.com/cumsauce/modeler/releases/tag/latest) release.
To give a Claude Code run in another repo's workflow the `mcp__modeler__*`
tools, add the action before it:

```yaml
jobs:
  claude:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: cumsauce/modeler@main        # installs it, writes .mcp.json
      - uses: anthropics/claude-code-action@v1
        with:
          anthropic_api_key: ${{ secrets.ANTHROPIC_API_KEY }}
          claude_args: --mcp-config .mcp.json --allowedTools "mcp__modeler__*"
```

The action adds `{"modeler": {"command": ..., "args": ["--headless",
"--project", <repo root>]}}` to `.mcp.json` (inputs: `release`, `mcp-config`,
`project`; output: `bin`). Headless means every tool but the renders
(`agent_views`, `screenshot`): build, `verify`, `inspect`, `probe_ray`,
`material_view` (CPU) and `export_model` all work, and relative exports land
in the project's `assets/`. Anywhere else on Linux, download the tarball from
the release and run `modeler --headless`.

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
  In a game project (a `Cargo.toml` beside `assets/`, a `.modeler/` folder,
  or `--project <dir>`) everything lives in `<project>/.modeler/`, outside
  `assets/`, and relative exports land in `assets/`. Elsewhere it uses the
  shared `~/Library/Application Support/friendslop/modeler/`.
- **Senses for agents**: `verify` checks (closed meshes, bounds, grounded,
  stable), `inspect` (gaps/overlaps between parts, cross-sections, mass and
  tipping), and `agent_views` renders: several views tiled into one image,
  part-ID colouring, dimension overlays and before/after diffs.
- **Procedural PBR materials**: node graphs of 2D/3D SDFs, noise (value,
  gradient, worley), maths and colour, evaluated in parallel on the CPU and
  baked to base colour / roughness / metallic / normal / AO textures. Tileable
  (real-size tiles mapped by metric uvs) or object-space (3D fields on a
  per-part atlas). Their own history trees, a `materials/` library next to the
  prefabs, deterministic checks, and textured glTF export.
  `cargo run --example procedural_material` walks through it.
- **Annotations**: comments pinned to a part/face, typed in the preview
  window or via tools.

## Preview window controls

The window opens on just the 3D view. Mouse drag orbits / pans, scroll
zooms; `O` toggles the outliner, `S` the style pane, `M` the material pane
(the active material's nodes with live thumbnails and a lit preview), and
`G` the grid: every open document side by side, labelled with the agent
that last changed it, so you can watch several agents at once (click a
tile to switch to it). Click a part then type to annotate (Enter commits,
Esc cancels).
