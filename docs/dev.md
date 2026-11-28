# Dev notes (server internals)


- Agents driving the server over stdio (frozen MCP tool list) should run
  `python3 mcp_driver.py --daemon --windowed` once in the background and then
  `python3 mcp_driver.py --connect --calls <file>` per batch: one long-lived
  server means the preview window stays open and state is live between
  invocations (autosave covers even a daemon restart). `{"name": "quit"}`
  stops it. The default throwaway mode spawns and kills a server per
  invocation — window flashes open/closed, state restored from autosave.

- **Several agents can share one daemon.** Each connection has its own
  thread and calls go to the server concurrently (responses are routed by
  JSON-RPC id), so one agent's slow render doesn't block another's quick
  call; edits still apply one at a time on the app thread. Every call carries its client (the agent
  session behind the calling shell, or `--client NAME`) in the MCP
  `_meta` (`modeler/client`, `modeler/doc` for `--doc`), and the server
  (`Session::begin_call` / `end_call`):
  - keeps each client's own active document, switching back when another
    client switched away (said once, then quietly);
  - credits history nodes and op writes to the client, tells it what other
    clients changed in its document since its last call ("since your last
    call, B made 2 edit(s) ..."), warns when a batch replaces an op another
    client wrote, and refuses an `undo` of another client's step unless
    `force: true`;
  - `--doc NAME` pins a run to a document (opened, or new, if not open).
  Calls without `_meta` (a plain MCP client, the human's window) behave as
  before. Separate servers sharing one catalog are guarded by the session
  lock (autosave) and `save_prefab`'s changed-file check.
- `apply_ops` `expect` and the `verify` tool run the checks once
  (`Session::verify_run` returns checks plus report); never call
  `verify_checks` and `verify` back to back.
- **Slow calls run off the app thread** (`server/tools.rs`), so the window
  keeps drawing. `apply_ops` takes a `Session::snapshot` (the open
  documents without history, parts shared, plus the working materials) in
  a short app-thread closure (which also reads `ops_file` /
  `from_reference` images, after begin_call set the project), applies the
  same batch to that copy on a `spawn_blocking` worker to fill the op
  cache, then runs the real apply on the app thread, which is all cache
  hits (a 5 s sdf edit costs the frame ~3 ms). The worker's result is
  discarded, so atomicity, rollback, history, authorship and undo guards
  are exactly the real apply's; a stale copy only costs cache misses.
  A per-document tokio lock (`Tools::doc_lock`) keeps one document's
  `apply_ops` calls in order from snapshot to commit (and through its
  `expect`); other documents and other tools proceed meanwhile. The
  commit re-activates the call's document (`Session::ensure_active`) if
  another call switched away, and errors if it was closed. `verify`,
  `inspect` and `apply_ops`' `expect` run read-only on a snapshot on a
  worker (`run_detached_json`). Other tools (material_ops, style, undo,
  export, renders) still run on the app thread. New slow read-only tools
  should use `run_detached_json`; new slow mutating ones, the
  warm-then-commit pattern.
- The daemon keeps the server's stderr (panics, bake errors) in
  `<catalog dir>/daemon.log` (rotated to `daemon.log.1` past 1 MB at each
  spawn; `MODELER_STDERR=<file>` overrides). The server's last line there
  is `modeler: exit: <reason>` (window closed / stdin EOF (MCP client gone)
  / panic in thread ...), which the restart note quotes.
- **One main autosaver per catalog dir**: at startup the server writes
  `<catalog dir>/session.lock` with its pid. A second live server finds it
  held and autosaves to its own **slot** instead: `session.<slot>.json`,
  `open_docs.<slot>.json` and the materials' `session.<slot>.json`
  (`MODELER_SLOT`, which the driver sets per daemon socket, else
  `pid<pid>`). On startup it restores from its slot when that exists, so a
  restarted second daemon gets its work back. Its stderr says "autosave
  SLOT", `scene_summary` has an `autosave:` line, and the driver tells each
  client once in its first result. A dead holder's lock is taken over, also
  mid-session (from then on it writes the main files); a clean exit removes
  it. A slot server also holds `session.<slot>.lock`. At startup the main
  daemon (the lock's owner) moves the autosaves of slot servers that are
  gone (`catalog::archive_dead_slots`: the slot lock's pid is dead, a
  `pid<N>` slot's pid is dead, or, with no lock, none of its files changed
  for a day) into `archive/slot-<slot>/` beside them (prefabs and
  materials), never deleting them; its own files and live slots stay.
- **Parked documents** (`src/parked.rs`, `topic:scenes`): one file per
  document under `parked/<project>-<hash>/`, line 1 the index entry, line 2
  the body; startup reads line 1s only, keeps 5 history nodes, drops ones
  equal to their catalog file and archives ones parked over 7 days ago.
  History journals are capped at 64 MB (`topic:history`).
- rmcp 1.8 server runs a tokio current-thread on a spawned thread; the Bevy
  app owns the main thread (winit on macOS). Bridge = tokio mpsc + oneshot.
- `--headless` uses MinimalPlugins — don't add ScheduleRunnerPlugin again.
  `--offscreen` is DefaultPlugins with no primary window and WinitPlugin
  disabled, driven by ScheduleRunnerPlugin at 60 Hz; it exits on stdin EOF
  like headless.
- Lints: `clippy::unwrap_used` and `expect_err` are denied — use
  `let Ok(..) = .. else`, `unwrap_or_else(|e| panic!("{e}"))` in tests, and
  return ids from APIs instead of unwrapping options.
- `bevy::shape` (not `bevy::math::primitives`) is where `Circle`/`Annulus`
  live; `Annulus` fields are `inner_circle`/`outer_circle`.
- f32: never assert derived coordinates with `round()` (0.49999997 rounds to
  0) — compare with a 1e-5 tolerance.
- clippy denies `chunks_exact(3)` with a const chunk — use
  `as_chunks::<3>().0`.
- In tests that hit the catalog, set `FRIENDSLOP_DATA_DIR` to a tempdir (the
  IO is funneled through `catalog_dir()` for exactly this).
- Bevy 0.20: `MeshRayCast` borrows `Assets<Meshes>` (defer pin spawns);
  `AccumulatedMouseScroll` for the wheel; winit sends `Key::Space`, not a
  space character.
- **Restart flow** (after code changes): `cargo build`, then
  `pkill -f target/debug/modeler` — the shared daemon (behind the MCP
  proxy) respawns the server from the binary on disk on the next tool call. The respawned session AUTOSAVES
  and RESTORES: mutations are written to `session.json` in the catalog dir
  0.5 s after the last edit (and on exit; a hard kill loses at most that), and a server started with `--restore` (the driver always does) parks it on startup, so
  unsaved WIP survives restarts (`scene restore` NAME reopens it). `new_model` starts clean (a non-empty old
  model stays open beside it, also in the autosave). `--model <path>` skips the
  restore and loads that file instead. The MCP tool list is fixed per
  connection, so newly added tools only appear in a NEW session.

