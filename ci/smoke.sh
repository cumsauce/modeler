#!/usr/bin/env bash
# Speaks MCP to the modeler on stdio: initialize, list the tools, build a
# box and verify it. With `render`, runs --offscreen and also renders an
# agent view (on a software GPU in CI).
#
#   ci/smoke.sh path/to/modeler [render]
set -euo pipefail
bin="${1:-modeler}"
mode="${2:-}"
work="$(mktemp -d)"
export FRIENDSLOP_DATA_DIR="$work/data"
out="$work/out.jsonl"
flag=--headless
[ "$mode" = render ] && flag=--offscreen

mkfifo "$work/in"
"$bin" "$flag" < "$work/in" > "$out" 2> "$work/err.log" &
pid=$!
exec 3> "$work/in"

# Waits (up to $2 s) for the response with id $1.
wait_for() {
  for _ in $(seq 1 "$2"); do
    grep -q "\"id\":$1[,}]" "$out" 2>/dev/null && return 0
    kill -0 "$pid" 2>/dev/null || break
    sleep 1
  done
  echo "no response to request $1"; cat "$out"; tail -40 "$work/err.log"; exit 1
}
send() { echo "$1" >&3; }

send '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}'
wait_for 1 60
send '{"jsonrpc":"2.0","method":"notifications/initialized"}'
send '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
send '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"apply_ops","arguments":{"ops":[{"type":"primitive","name":"crate","kind":"box","size":0.5,"translation":[0,0.25,0]},{"type":"material","name":"crate","color":"#b5793a","flat":true}]}}}'
send '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"verify","arguments":{"grounded":true,"validate":true}}}'
wait_for 4 60
grep -q '"apply_ops"' "$out" || { echo "tools/list has no apply_ops"; exit 1; }
grep '"id":4' "$out" | grep -q 'PASS grounded' || { echo "verify did not pass"; cat "$out"; exit 1; }

if [ "$mode" = render ]; then
  send '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"agent_views","arguments":{"views":["front","iso"],"width":320,"height":240}}}'
  wait_for 5 300
  line="$(grep '"id":5' "$out")"
  if ! echo "$line" | grep -q '"type":"image"'; then
    echo "agent_views returned no image:"; echo "$line" | cut -c1-2000; tail -40 "$work/err.log"; exit 1
  fi
  # Save the picture for the CI artifact.
  echo "$line" | python3 -c 'import sys,json,base64
r=json.load(sys.stdin)["result"]["content"]
img=[c for c in r if c.get("type")=="image"][0]
open(sys.argv[1],"wb").write(base64.b64decode(img["data"]))' "${SMOKE_IMAGE:-$work/view.png}"
  echo "smoke: rendered $(echo "$line" | grep -o '"data":"[^"]*' | wc -c) base64 bytes"
fi

exec 3>&-
wait "$pid" || true
echo "smoke: $flag MCP ok"
