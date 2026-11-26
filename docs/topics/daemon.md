# Daemon, proxy and sessions

**One server, one window for every session.** The MCP config runs
`python3 mcp_driver.py --proxy`, not the binary: each Claude session gets a
thin stdio proxy, and all proxies share one daemon (started windowed on
first use, detached, so it outlives the sessions). The proxy runs every
request on its own daemon connection, so concurrent calls from subagents of
one session never queue behind a slow one, and while a `tools/call` that
carried `_meta.progressToken` runs it sends `notifications/progress` every
5 s (`MODELER_PROGRESS_EVERY`), which resets the client's tool timeout. A
client-side timeout does not mean the call failed: the server keeps working,
so check `history` / `scene_summary` before retrying an `apply_ops`. The `mcp__modeler__*`
tools and the driver below therefore talk to the same server and the same
preview window. Each session is its own client (per-client active
documents, authorship, undo guards). Closing the window ends the server;
the next call restarts it from autosave. **Every project shares that one
daemon** (`/tmp/modeler-mcp.sock`): each session's proxy names its project
(the game project its working directory is in, or none for the shared
library; from a repo root, the one `.modeler/` project up to three levels
below, so a session in `friendSLOP/` works in `friendSLOP/crates/game`) in
every call's `_meta` (`modeler/project`), and the server
switches the catalog, exports and prefab lookups to it for that call
(`catalog::set_call_project`; autosaves and the session lock stay in the
server's own home library). Documents remember their project
(`Model.project`, autosave only), a document of the same name from another
project is refused rather than taken over, and `new_model` starts a
document beside any non-empty one instead of replacing it. **Subagents of one Claude session share one proxy, so one client id and one active document: pass `doc` (an open document's name) on every tool that reads or changes a document (`apply_ops`, `verify`, `inspect`, `agent_views`, `scene_summary`, `ops`, `op_get`, `probe_ray`, `screenshot`, `set_camera`, `style`, `material_ops`, the annotation tools, `history`, `undo`, `redo`, `jump`, `diff`, `cherry_pick`, `history_prune`, `set_model_name`, `save_prefab`, `export_model`); the call errors if no document of that name exists.** A `doc` naming a document that was idle-closed, parked after a restart, or is a catalog prefab in the call's project is opened again automatically (the result starts with a `note: opened ... from the catalog` line); that never changes another client's active document or the human's view. An unknown name errors, with a did-you-mean hint. `save_prefab` also refuses (unless `force`) a name another open document holds. **`new_model` never displaces work**: unless the current document is empty (no ops, no parts) it stays open and the new one starts beside it, so `doc` reaches either; only an empty document is replaced, and a name that is already open is refused (switch to it or pass `doc`).

**Shared daemon or one per agent.** Share the one daemon (the default)
when the human should watch every agent in one window and agents may
reach each other's documents. Run one per agent when agents must not see
each other's open documents, or one agent's heavy renders shouldn't share
a server with another's: set `MODELER_SOCKET=/tmp/modeler-<name>.sock` in
that session's MCP config `env` (or pass `--socket PATH`; keep the path
short, Unix sockets are limited to ~100 bytes). Each socket gets its own
daemon, server, log (`daemon.<slot>.log`) and autosave slot
(`session.<slot>.json`; the first server in a catalog dir holds the main
`session.lock`), and the catalog is shared, with `save_prefab`'s
changed-file check guarding writes. Caveat: separate windows and separate
open documents, so a `prefab` item only sees the other daemon's work once
it is saved to the catalog. `MODELER_BINARY` overrides the server binary
(default `target/debug/modeler` beside `mcp_driver.py`).

**Working efficiently.** Verify numerically (`verify`, `inspect`) between
steps and render once per model at the end with one tiled `agent_views`
(`["front", "right", "top", "iso"]`) rather than after every step. For
many identical props in a map (hundreds of desk fans), model one prefab
(the fan, with its rotor clip) and place `prefab` instances, or bake a set
into one prefab with arrays: never hundreds of separate items. Prefab
instances export by reference in a `.bsn`. A new agent's first call brings the window to the front (not once the human has picked a document).
After `cargo build`, restart the
server with `pkill -f target/debug/modeler` (the daemon respawns it on the
next call) or stop everything with a `quit` call.

Without MCP tools (or for scripted batches), use the driver directly:

```sh
cargo build                # or skip if target/debug/modeler is current
python3 mcp_driver.py --daemon --windowed &   # skip if the shared daemon is up
```

Without `--windowed` the daemon runs `--offscreen`: the full renderer with no
window. `agent_views` works, and `screenshot` renders the window camera's
view offscreen (1280x800, returned 960 wide unless `detail: "high"` or `width`;
`set_camera` and auto-framing steer it as usual).
`--headless` drops rendering entirely.

Then talk to it with `python3 mcp_driver.py --connect --calls <file>` (JSON
array of `{"name", "args"}`), one batch per step, and `{"name": "quit"}` when
done. If the daemon's server has died (killed, crashed), the next call
restarts it from the autosave and says why it had exited ("code 0: window
closed"); with no daemon at all the client prints the start command, and a
second `--daemon` on the same socket refuses to start ("a modeler daemon is
already running"). `--save-shot PATH` names the run's first image PATH and
every later one PATH_2, PATH_3, ...; each result lists `saved_images`. Typical first calls: `scene_summary` (also shows the autosaved session,
if any), `catalog_list`, then `new_model` + `apply_ops` to build. If your
session's MCP tool list has no `mcp__modeler__*` tools, this driver is the
documented fallback — it speaks the same tool surface over stdio. Each
result line is `{"ok", "text", "json"?, ...}`; `--json` drops the prose when
structured JSON is present. The daemon keeps the preview window open; check
it with `agent_views` / `screenshot` and verify numerically with `verify`.

**In CI** (Linux, no GPU): the `cumsauce/modeler@main` action installs the
`latest` release build and adds `modeler --headless --project <repo>` to
`.mcp.json` (README, "In CI"). Headless runs every tool except the renders
(`agent_views`, `screenshot`); with the action's `render: true` it runs
`--offscreen` on Mesa's software Vulkan (no GPU) and renders work too, a few
seconds each.
