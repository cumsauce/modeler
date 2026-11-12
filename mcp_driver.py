#!/usr/bin/env python3
"""Drive the modeler MCP server over stdio (fallback while the session's
MCP tool list is frozen without mcp__modeler__* tools).

Two modes:

Daemon (keeps ONE modeler process — and its preview window — alive across
many driver invocations):
    mcp_driver.py --daemon [--windowed | --headless] [--socket PATH]
        spawns the modeler server (offscreen by default: agent_views render,
        no window; --windowed adds the preview window, --headless no renderer), listens on a unix socket, executes tool
        calls from any number of connected clients (each on its own thread;
        calls interleave one at a time, so a long batch doesn't block other
        agents). Stays up when clients disconnect; `quit` stops it.

Client (connects to the daemon, or spawns a throwaway server):
    mcp_driver.py --connect [--socket PATH] --calls calls.json [--save-shot p]
    mcp_driver.py --calls calls.json ...          # throwaway mode, as before
Several agents may share one daemon: every call carries its client (the
agent session behind the calling shell, or --client NAME) to the server,
which keeps each client's own active document (switching back, and saying
so, when another client switched away), credits history steps to it, tells
it what other clients changed in its document since its last call, refuses
an undo that would revert another client's step (undo force: true
overrides) and warns when a batch replaces ops another client wrote.
--doc NAME pins the document for this run (opened from the catalog, or new,
if not open).
Image options: --save-shot PATH (the run's first image to PATH, every later
image from any call of the same client run to PATH_2, PATH_3, ...),
--save-dir DIR (every image, numbered), --b64 (also print base64; off by default).
--json: when a tool returns structured content (scene_summary, apply_ops,
verify), print it as "json" and drop the prose "text" (structured content is
always included as "json"; --json only removes the duplicate prose).
Calls JSON: [{"name": "...", "args": {...}}, ...]
Without --calls in client/throwaway mode, reads calls line-by-line on stdin;
"quit" in daemon mode stops the daemon.

Results print as JSON lines: {"ok":..., "text": [...], "json": {...}?,
"n_images": ..., "saved_images": [paths]}.

The daemon keeps the server's stderr in <catalog dir>/daemon.log (rotated at
1 MB; MODELER_STDERR=<file> overrides) and, when the server has died, its
restart note quotes the server's last "modeler: exit: <reason>" line.
"""
import json
import subprocess, sys, base64, os, signal, socket, stat, time

BINARY = "/Users/lume/modeler/target/debug/modeler"
SOCKET = "/tmp/modeler-mcp.sock"
LOG_ROTATE_BYTES = 1 << 20
EXIT_PREFIX = "modeler: exit: "


def daemon_log_path():
    """MODELER_STDERR if set, else <catalog dir>/daemon.log (the server says
    where its catalog dir is, so project / FRIENDSLOP_DATA_DIR rules hold)."""
    log = os.environ.get("MODELER_STDERR")
    if log:
        return log
    try:
        out = subprocess.run([BINARY, "--print-catalog-dir"], capture_output=True,
                             text=True, timeout=30)
        d = out.stdout.strip()
    except Exception:
        d = ""
    if not d:
        return None
    os.makedirs(d, exist_ok=True)
    path = os.path.join(d, "daemon.log")
    try:
        if os.path.getsize(path) > LOG_ROTATE_BYTES:
            os.replace(path, path + ".1")
    except OSError:
        pass
    return path


class ShotNamer:
    """Names --save-shot images: one counter per client run, so the first
    image goes to PATH and every later one (any call) to PATH_2, PATH_3 ..."""

    def __init__(self):
        self.count = 0

    def next(self, save_shot):
        self.count += 1
        if self.count == 1:
            return save_shot
        stem, ext = os.path.splitext(save_shot)
        return f"{stem}_{self.count}{ext or '.png'}"


class ModelerSession:
    """Owns the modeler process and the JSON-RPC conversation."""

    def __init__(self, mode="headless", log=None, slot=None):
        # mode: "windowed" (preview window), "offscreen" (renders agent views,
        # no window), "headless" (no renderer at all).
        cmd = [BINARY] + {"windowed": [], "offscreen": ["--offscreen"]}.get(mode, ["--headless"])
        cmd.append("--restore")  # daemon restarts and throwaway runs pick up the autosave
        env = dict(os.environ)
        if slot:
            # A server that does not own the main autosave writes
            # session.<slot>.json; a stable slot lets a restart find it.
            env["MODELER_SLOT"] = slot
        # `log` (the daemon's daemon.log, or MODELER_STDERR=<path>) keeps the
        # server's stderr (panics, bake errors, the exit reason) instead of
        # discarding it.
        log = log or os.environ.get("MODELER_STDERR")
        self.log = log
        self.log_start = 0
        if log:
            err = open(log, "ab")
            self.log_start = err.tell()  # this run's lines start here
        else:
            err = subprocess.DEVNULL
        self.proc = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=err, bufsize=0, env=env)
        if log:
            err.close()  # the child holds its own handle
        self.next_id = 0
        self.shots = ShotNamer()
        self._handshake()
        self.slot_note = self._slot_note()

    def _slot_note(self):
        """When another server owns the main autosave, the server says so on
        stderr at startup ("modeler: autosave SLOT ..."): return a note."""
        if not self.log:
            return None
        try:
            with open(self.log, "rb") as f:
                f.seek(self.log_start)
                for line in f.read().decode("utf-8", "replace").splitlines():
                    if line.startswith("modeler: autosave SLOT"):
                        return ("daemon: " + line[len("modeler: "):]
                                + " (restored on restart; save_prefab to share work)")
        except OSError:
            pass
        return None

    def exit_reason(self):
        """Why the server exited: its own "modeler: exit:" line from this
        run's log, else the signal that killed it."""
        code = self.proc.returncode
        reason = None
        if self.log:
            try:
                with open(self.log, "rb") as f:
                    f.seek(self.log_start)
                    for line in f.read().decode("utf-8", "replace").splitlines():
                        if line.startswith(EXIT_PREFIX):
                            reason = line[len(EXIT_PREFIX):].strip()
            except OSError:
                pass
        if reason is None and code is not None and code < 0:
            try:
                reason = f"killed by {signal.Signals(-code).name}"
            except ValueError:
                reason = f"killed by signal {-code}"
        if reason is None:
            reason = "no exit reason logged" + (f", see {self.log}" if self.log else "")
        return f"code {code}: {reason}"

    def _send(self, obj):
        self.proc.stdin.write((json.dumps(obj) + "\n").encode())
        self.proc.stdin.flush()

    def _recv(self):
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("server closed stdout")
        return json.loads(line)

    def _handshake(self):
        self._send({"jsonrpc": "2.0", "id": self.next_id, "method": "initialize",
                    "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                               "clientInfo": {"name": "mcp-driver", "version": "0"}}})
        self._recv()
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        self.next_id += 1

    def tool_names(self):
        rid = self.next_id; self.next_id += 1
        self._send({"jsonrpc": "2.0", "id": rid, "method": "tools/list", "params": {}})
        while True:
            resp = self._recv()
            if resp.get("id") == rid:
                return sorted(t["name"] for t in resp["result"]["tools"])

    def call(self, name, args=None, save_shot=None, save_dir=None, b64=False, shots=None,
             meta=None):
        rid = self.next_id; self.next_id += 1
        params = {"name": name, "arguments": args or {}}
        if meta:
            params["_meta"] = meta  # which agent is calling (see daemon_mode)
        self._send({"jsonrpc": "2.0", "id": rid, "method": "tools/call", "params": params})
        while True:
            resp = self._recv()
            if resp.get("id") != rid:
                continue  # notifications
            if "error" in resp:  # JSON-RPC protocol error (bad args, unknown tool, ...)
                err = resp["error"]
                return {"ok": False,
                        "text": [f"{err.get('message', 'error')}: {err.get('data', '')}"],
                        "n_images": 0}
            result = resp.get("result", {})
            texts, images = [], []
            for c in result.get("content", []):
                if c.get("type") == "text":
                    texts.append(c.get("text", ""))
                elif c.get("type") == "image":
                    images.append(c)
            out = {"ok": not result.get("isError", False), "text": texts,
                   "n_images": len(images), "saved_images": []}
            if result.get("structuredContent") is not None:
                out["json"] = result["structuredContent"]
            if images:
                if b64:  # opt-in: the base64 blobs drown the useful output
                    out["images_b64"] = [i.get("data", "") for i in images]
                saved = []
                if save_shot:
                    # The run's first image to the exact path; every later one
                    # (this call or a later call) gets _2, _3 ... suffixes.
                    namer = shots or self.shots
                    for img in images:
                        path = namer.next(save_shot)
                        with open(path, "wb") as f:
                            f.write(base64.b64decode(img["data"]))
                        saved.append(path)
                if save_dir:
                    os.makedirs(save_dir, exist_ok=True)
                    self.shot_seq = getattr(self, "shot_seq", 0) + 1
                    for k, img in enumerate(images):
                        path = os.path.join(save_dir, f"{self.shot_seq:03d}-{name}-{k + 1}.png")
                        with open(path, "wb") as f:
                            f.write(base64.b64decode(img["data"]))
                        saved.append(path)
                out["saved_images"] = saved
            return out

    def close(self):
        # EOF first: a headless server exits on its own (logging its exit
        # line and dropping its session lock); a windowed one is terminated.
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.proc.terminate()


def trim(out, json_only):
    """--json: drop the prose when structured content is present."""
    if json_only and "json" in out:
        out = dict(out)
        out.pop("text", None)
    return out


def run_calls(session, calls, save_shot=None, save_dir=None, b64=False, json_only=False):
    for c in calls:
        if isinstance(c, str):
            print(json.dumps({"ok": True, "text": [c], "n_images": 0, "saved_images": []}),
                  flush=True)
            continue
        out = session.call(c["name"], c.get("args", {}), save_shot, save_dir, b64)
        print(json.dumps(trim(out, json_only)), flush=True)


def claim_socket(socket_path):
    """Refuses to start over a live daemon; removes only a stale socket."""
    try:
        st = os.stat(socket_path)
    except FileNotFoundError:
        return
    if not stat.S_ISSOCK(st.st_mode):
        print(f"{socket_path} exists and is not a socket; remove it or pass --socket",
              flush=True)
        sys.exit(1)
    # A refused connect means nobody listens (or, on macOS, a full backlog):
    # ask a few times before calling the socket stale.
    for attempt in range(3):
        probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            probe.connect(socket_path)
            break  # something answered
        except FileNotFoundError:
            return
        except ConnectionRefusedError:
            if attempt == 2:
                os.unlink(socket_path)  # nobody listening: a dead daemon's leftover
                return
            time.sleep(0.2)
        except OSError as e:
            print(f"cannot check {socket_path} ({e.strerror}); not starting", flush=True)
            sys.exit(1)
        finally:
            probe.close()
    print(f"a modeler daemon is already running on {socket_path}", flush=True)
    sys.exit(1)


def client_identity():
    """Who is calling: the agent session behind this shell. Each agent's
    commands run in fresh shells under one long-lived process, so the
    shell's parent identifies the session; --client NAME overrides."""
    if "--client" in sys.argv:
        return sys.argv[sys.argv.index("--client") + 1]
    try:
        out = subprocess.run(["ps", "-o", "ppid=", "-p", str(os.getppid())],
                             capture_output=True, text=True, timeout=2).stdout.strip()
        return f"pid:{int(out)}"
    except Exception:
        return f"pid:{os.getppid()}"


def daemon_slot(socket_path):
    """A stable autosave slot per daemon socket."""
    import hashlib
    return "d" + hashlib.sha1(os.path.abspath(socket_path).encode()).hexdigest()[:8]


def daemon_mode(socket_path, mode):
    claim_socket(socket_path)
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(socket_path)  # before spawning: a racing second daemon fails here
    srv.listen(16)
    log = daemon_log_path()
    try:
        first = ModelerSession(mode=mode, log=log, slot=daemon_slot(socket_path))
    except Exception:
        srv.close()
        os.unlink(socket_path)
        raise
    print(f"modeler daemon ready on {socket_path}"
          + (f" (server log: {log})" if log else ""), flush=True)
    # Several agents share one daemon. Each connection gets its own thread
    # and the calls interleave one at a time (a lock around the server), so
    # one agent's long batch doesn't hold every other agent at the door.
    # Every call carries its agent in `_meta` ("modeler/client", plus
    # "modeler/doc" for --doc): the server keeps each agent's own active
    # document, credits history steps to it, and tells it what other agents
    # changed in its document since its last call.
    import threading
    state = {"session": first, "stop": False}
    lock = threading.Lock()
    told_slot = set()  # clients already told this server autosaves to a slot

    def serve(conn):
        shots = ShotNamer()  # one --save-shot counter per client run
        client, pin, pending_note = None, None, None
        with conn:
            f = conn.makefile("r")
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    c = json.loads(line)
                    if isinstance(c, dict) and "hello" in c:
                        h = c["hello"]
                        client, pin = h.get("client"), h.get("doc")
                        with lock:
                            session = state["session"]
                            if session.slot_note and (client, session.proc.pid) not in told_slot:
                                told_slot.add((client, session.proc.pid))
                                pending_note = session.slot_note
                        continue
                    is_quit = c == "quit" or (isinstance(c, dict) and c.get("name") == "quit")
                    if is_quit:
                        conn.sendall((json.dumps(
                            {"ok": True, "text": ["daemon stopping"],
                             "n_images": 0, "saved_images": []}) + "\n").encode())
                        state["stop"] = True
                        try:  # wake accept()
                            w = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                            w.connect(socket_path)
                            w.close()
                        except OSError:
                            pass
                        return
                    note = None
                    meta = {}
                    if client:
                        meta["modeler/client"] = client
                    if pin:
                        meta["modeler/doc"] = pin
                    with lock:
                        session = state["session"]
                        if session.proc.poll() is not None:
                            # The modeler exited (killed, crashed, rebuilt):
                            # respawn it; autosave restores the session.
                            why = session.exit_reason()
                            session = ModelerSession(mode=mode, log=daemon_log_path(),
                                                     slot=daemon_slot(socket_path))
                            state["session"] = session
                            note = (f"daemon: the modeler server had exited ({why}); "
                                    "restarted it, session restored from autosave"
                                    + (" (its own slot file)" if session.slot_note else ""))
                        out = session.call(c["name"], c.get("args", {}),
                                           c.get("save_shot"), c.get("save_dir"),
                                           c.get("b64", False), shots, meta)
                    if pending_note:
                        note = f"{note}\n{pending_note}" if note else pending_note
                        pending_note = None
                    if note:
                        out["text"] = [note] + list(out.get("text", []))
                except Exception as e:  # bad line or dead server
                    msg = f"daemon error: {e}"
                    session = state["session"]
                    if session.proc.poll() is not None:
                        msg += f" (the modeler server exited: {session.exit_reason()})"
                    out = {"ok": False, "text": [msg], "n_images": 0, "saved_images": []}
                try:
                    conn.sendall((json.dumps(out) + "\n").encode())
                except OSError:
                    return  # the client went away mid-call

    try:
        while not state["stop"]:
            conn, _ = srv.accept()
            if state["stop"]:
                conn.close()
                break
            threading.Thread(target=serve, args=(conn,), daemon=True).start()
    finally:
        with lock:
            state["session"].close()
        srv.close()
        if os.path.exists(socket_path):
            os.unlink(socket_path)


def client_mode(socket_path, calls_path, save_shot, save_dir=None, b64=False, json_only=False):
    calls = json.load(open(calls_path)) if calls_path != "/dev/stdin" else json.load(sys.stdin)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        s.connect(socket_path)
    except OSError as e:
        print(json.dumps({"ok": False, "text": [
            f"no modeler daemon on {socket_path} ({e.strerror}). Start one with: "
            "python3 mcp_driver.py --daemon --windowed &  (or without --windowed: agent views, no window)"], "n_images": 0,
            "saved_images": []}), flush=True)
        sys.exit(1)
    with s:
        f = s.makefile("r")
        hello = {"client": client_identity()}
        if "--doc" in sys.argv:
            hello["doc"] = sys.argv[sys.argv.index("--doc") + 1]
        s.sendall((json.dumps({"hello": hello}) + "\n").encode())
        for c in calls:
            payload = dict(c)
            if save_shot:
                payload["save_shot"] = os.path.abspath(save_shot)  # the daemon's cwd differs
            if save_dir:
                payload["save_dir"] = os.path.abspath(save_dir)
            if b64:
                payload["b64"] = True
            s.sendall((json.dumps(payload) + "\n").encode())
            line = f.readline().strip()
            try:
                print(json.dumps(trim(json.loads(line), json_only)), flush=True)
            except ValueError:
                print(line, flush=True)


def main():
    argv = sys.argv
    daemon = "--daemon" in argv
    connect = "--connect" in argv
    windowed = "--windowed" in argv
    # The daemon renders agent views by default (offscreen, no window);
    # --windowed adds the preview window, --headless drops rendering.
    mode = "windowed" if windowed else ("headless" if "--headless" in argv else "offscreen")
    socket_path = SOCKET
    if "--socket" in argv:
        socket_path = argv[argv.index("--socket") + 1]
    save_shot = None
    if "--save-shot" in argv:
        save_shot = argv[argv.index("--save-shot") + 1]
    save_dir = None
    if "--save-dir" in argv:
        save_dir = argv[argv.index("--save-dir") + 1]
    b64 = "--b64" in argv
    json_only = "--json" in argv
    calls_path = None
    if "--calls" in argv:
        calls_path = argv[argv.index("--calls") + 1]

    if daemon:
        daemon_mode(socket_path, mode)
    elif connect:
        client_mode(socket_path, calls_path, save_shot, save_dir, b64, json_only)
    else:
        # Throwaway mode: own server, dies with this invocation.
        session = ModelerSession(mode="windowed" if windowed else "headless")
        print(json.dumps({"tools": session.tool_names()}), flush=True)
        try:
            if calls_path:
                run_calls(session, json.load(open(calls_path)), save_shot, save_dir, b64,
                          json_only)
            else:
                for line in sys.stdin:
                    line = line.strip()
                    if not line or line == "quit":
                        break
                    c = json.loads(line)
                    out = session.call(c["name"], c.get("args", {}), save_shot, save_dir, b64)
                    print(json.dumps(trim(out, json_only)), flush=True)
        finally:
            session.close()


if __name__ == "__main__":
    main()
