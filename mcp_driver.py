#!/usr/bin/env python3
"""Drive the modeler MCP server over stdio (fallback while the session's
MCP tool list is frozen without mcp__modeler__* tools). Spawns the server,
runs a JSON-RPC tool-call sequence, saves screenshot results as PNGs, and
prints results as JSON lines.

Usage: mcp_driver.py <binary> [--save-shot <png-path>] [--calls <calls.json>]
Calls JSON: [{"name": "op_primitive", "args": {...}}, ...]
Without --calls, runs an interactive newline-delimited protocol:
  {"name": ..., "args": {...}}  per line on stdin
  -> {"ok": true, "text": [...]} / {"ok": false, "error": ...} per line on stdout
  "quit" exits.
"""
import json, subprocess, sys, base64, threading, os

BINARY = sys.argv[1] if len(sys.argv) > 1 else "/Users/lume/modeler/target/debug/modeler"

def parse_args(argv):
    save_shot = None
    calls_path = None
    windowed = False
    i = 2
    while i < len(argv):
        if argv[i] == "--save-shot":
            i += 1; save_shot = argv[i]
        elif argv[i] == "--calls":
            i += 1; calls_path = argv[i]
        elif argv[i] == "--windowed":
            windowed = True
        i += 1
    return save_shot, calls_path, windowed

def main():
    save_shot, calls_path, windowed = parse_args(sys.argv)
    cmd = [BINARY] + ([] if windowed else ["--headless"])
    proc = subprocess.Popen(
        cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL, bufsize=0)
    next_id = [0]

    def send(obj):
        proc.stdin.write((json.dumps(obj) + "\n").encode())
        proc.stdin.flush()

    def recv():
        line = proc.stdout.readline()
        if not line:
            raise RuntimeError("server closed stdout")
        return json.loads(line)

    # MCP initialize handshake
    send({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2024-11-05", "capabilities": {},
        "clientInfo": {"name": "mcp-driver", "version": "0"}}})
    recv()
    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    next_id[0] = 1

    # Confirm tool list once.
    send({"jsonrpc": "2.0", "id": next_id[0], "method": "tools/list", "params": {}})
    resp = recv()
    tool_names = sorted(t["name"] for t in resp["result"]["tools"])
    print(json.dumps({"tools": tool_names}), flush=True)
    next_id[0] += 1

    def call(name, args=None):
        rid = next_id[0]; next_id[0] += 1
        send({"jsonrpc": "2.0", "id": rid, "method": "tools/call", "params": {
            "name": name, "arguments": args or {}}})
        while True:
            resp = recv()
            if resp.get("id") != rid:
                continue  # skip notifications
            result = resp.get("result", {})
            texts, images = [], []
            for c in result.get("content", []):
                if c.get("type") == "text":
                    texts.append(c.get("text", ""))
                elif c.get("type") == "image":
                    images.append(c)
            out = {"isError": result.get("isError", False), "text": texts,
                   "n_images": len(images)}
            if images and save_shot:
                with open(save_shot, "wb") as f:
                    f.write(base64.b64decode(images[0]["data"]))
                out["saved_shot"] = save_shot
            elif images:
                out["b64"] = images[0]["data"]
            print(json.dumps(out), flush=True)
            return out

    if calls_path:
        calls = json.load(open(calls_path))
        for c in calls:
            call(c["name"], c.get("args", {}))
    else:
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            if line == "quit":
                break
            c = json.loads(line)
            call(c["name"], c.get("args", {}))
    proc.stdin.close()
    proc.terminate()

if __name__ == "__main__":
    main()
