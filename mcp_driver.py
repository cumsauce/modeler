#!/usr/bin/env python3
"""Drive the modeler MCP server over stdio (fallback while the session's
MCP tool list is frozen without mcp__modeler__* tools).

Two modes:

Daemon (keeps ONE modeler process — and its preview window — alive across
many driver invocations):
    mcp_driver.py --daemon [--windowed] [--socket PATH]
        spawns the modeler server, listens on a unix socket, executes tool
        calls from any connecting client (one at a time). Stays up when
        clients disconnect; `quit` as a call shuts it down.

Client (connects to the daemon, or spawns a throwaway server):
    mcp_driver.py --connect [--socket PATH] --calls calls.json [--save-shot p]
    mcp_driver.py --calls calls.json ...          # throwaway mode, as before
Image options: --save-shot PATH (first image to PATH, extras PATH_2...),
--save-dir DIR (every image, numbered), --b64 (also print base64; off by default).
--json: when a tool returns structured content (scene_summary, apply_ops,
verify), print it as "json" and drop the prose "text" (structured content is
always included as "json"; --json only removes the duplicate prose).
Calls JSON: [{"name": "...", "args": {...}}, ...]
Without --calls in client/throwaway mode, reads calls line-by-line on stdin;
"quit" in daemon mode stops the daemon.

Results print as JSON lines: {"ok":..., "text": [...], "json": {...}?,
"n_images": ..., "saved_shot": path}.
"""
import json, subprocess, sys, base64, os, socket, threading

BINARY = "/Users/lume/modeler/target/debug/modeler"
SOCKET = "/tmp/modeler-mcp.sock"


class ModelerSession:
    """Owns the modeler process and the JSON-RPC conversation."""

    def __init__(self, windowed=False):
        cmd = [BINARY] + ([] if windowed else ["--headless"])
        self.proc = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, bufsize=0)
        self.next_id = 0
        self._handshake()

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

    def call(self, name, args=None, save_shot=None, save_dir=None, b64=False):
        rid = self.next_id; self.next_id += 1
        self._send({"jsonrpc": "2.0", "id": rid, "method": "tools/call",
                    "params": {"name": name, "arguments": args or {}}})
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
                   "n_images": len(images)}
            if result.get("structuredContent") is not None:
                out["json"] = result["structuredContent"]
            if images:
                if b64:  # opt-in: the base64 blobs drown the useful output
                    out["images_b64"] = [i.get("data", "") for i in images]
                saved = []
                if save_shot:
                    # First image to the exact path; extras get _2, _3 ... suffixes.
                    stem, ext = os.path.splitext(save_shot)
                    for k, img in enumerate(images):
                        path = save_shot if k == 0 else f"{stem}_{k + 1}{ext or '.png'}"
                        with open(path, "wb") as f:
                            f.write(base64.b64decode(img["data"]))
                        saved.append(path)
                    out["saved_shot"] = save_shot
                if save_dir:
                    os.makedirs(save_dir, exist_ok=True)
                    self.shot_seq = getattr(self, "shot_seq", 0) + 1
                    for k, img in enumerate(images):
                        path = os.path.join(save_dir, f"{self.shot_seq:03d}-{name}-{k + 1}.png")
                        with open(path, "wb") as f:
                            f.write(base64.b64decode(img["data"]))
                        saved.append(path)
                if saved:
                    out["saved_images"] = saved
            return out

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
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
            print(json.dumps({"ok": True, "text": [c], "n_images": 0}), flush=True)
            continue
        out = session.call(c["name"], c.get("args", {}), save_shot, save_dir, b64)
        print(json.dumps(trim(out, json_only)), flush=True)


def daemon_mode(socket_path, windowed):
    session = ModelerSession(windowed=windowed)
    if os.path.exists(socket_path):
        os.unlink(socket_path)
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(socket_path)
    srv.listen(1)
    print(f"modeler daemon ready on {socket_path}", flush=True)
    try:
        while True:
            conn, _ = srv.accept()
            with conn:
                f = conn.makefile("r")
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        c = json.loads(line)
                        is_quit = line == "quit" or c.get("name") == "quit"
                        if is_quit:
                            conn.sendall((json.dumps(
                                {"ok": True, "text": ["daemon stopping"],
                                 "n_images": 0}) + "\n").encode())
                            return
                        out = session.call(c["name"], c.get("args", {}),
                                           c.get("save_shot"), c.get("save_dir"),
                                           c.get("b64", False))
                    except Exception as e:  # bad line or dead server
                        out = {"ok": False, "text": [f"daemon error: {e}"],
                               "n_images": 0}
                    conn.sendall((json.dumps(out) + "\n").encode())
            # client disconnected: keep the modeler (and window) alive
    finally:
        session.close()
        if os.path.exists(socket_path):
            os.unlink(socket_path)


def client_mode(socket_path, calls_path, save_shot, save_dir=None, b64=False, json_only=False):
    calls = json.load(open(calls_path)) if calls_path != "/dev/stdin" else json.load(sys.stdin)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(socket_path)
    with s:
        f = s.makefile("r")
        for c in calls:
            payload = dict(c)
            if save_shot:
                payload["save_shot"] = save_shot
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
        daemon_mode(socket_path, windowed)
    elif connect:
        client_mode(socket_path, calls_path, save_shot, save_dir, b64, json_only)
    else:
        # Throwaway mode: own server, dies with this invocation.
        session = ModelerSession(windowed=windowed)
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
