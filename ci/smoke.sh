#!/usr/bin/env bash
# Speaks MCP to a headless modeler on stdio: initialize, list the tools,
# build a box and verify it. Usage: ci/smoke.sh path/to/modeler
set -euo pipefail
bin="${1:-modeler}"
work="$(mktemp -d)"
export FRIENDSLOP_DATA_DIR="$work/data"
out="$work/out.jsonl"
{
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}'
  echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  echo '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"apply_ops","arguments":{"ops":[{"type":"primitive","name":"crate","kind":"box","size":0.5,"translation":[0,0.25,0]}]}}}'
  echo '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"verify","arguments":{"grounded":true,"validate":true}}}'
  sleep 5
} | "$bin" --headless > "$out"
grep -q '"apply_ops"' "$out" || { echo "tools/list has no apply_ops"; cat "$out"; exit 1; }
grep '"id":4' "$out" | grep -q 'PASS grounded' || { echo "verify did not pass"; cat "$out"; exit 1; }
echo "smoke: headless MCP ok ($(grep -o '"name":"[a-z_]*"' "$out" | sort -u | wc -l) names seen)"
