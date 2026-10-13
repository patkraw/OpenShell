#!/usr/bin/env bash
# Print real gateway MCP exchanges (request, HTTP status, response) against a dev
# gateway. Used to write PROTOCOL.md. Needs: dev gateway, and a sandbox "sdk-lead"
# started with a grant (sdk/host_create.py creates one).
set -u
cd "$(dirname "$0")"
GW="${GW_URL:-http://127.0.0.1:18080/mcp}"
OS="${OPENSHELL_BIN:-../../target/debug/openshell} -g ${GATEWAY:-docker-dev}"
show() { # title, body
  echo "### $1"; echo; echo '```http'; echo "POST /mcp"; echo "$2"; echo '```'; echo
  echo '```http'
  curl -s -o /tmp/mcp-body.$$ -w 'HTTP %{http_code} %{content_type}\n' "$GW" -H 'content-type: application/json' -H 'accept: application/json, text/event-stream' -d "$2"
  python3 -c 'import json,sys; t=open(sys.argv[1]).read(); print(json.dumps(json.loads(t), indent=2) if t.strip() else "(no body)")' /tmp/mcp-body.$$
  echo '```'; echo
}
show "initialize" '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"doc","version":"0"}}}'
show "notifications/initialized (a notification: no id, no body back)" '{"jsonrpc":"2.0","method":"notifications/initialized"}'
show "tools/list as the user" '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
show "tools/call grant_list" '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"grant_list","arguments":{}}}'
show "tools/call with an unknown argument" '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"create_sandbox","arguments":{"nmae":"typo"}}}'
show "unknown method" '{"jsonrpc":"2.0","id":5,"method":"resources/list"}'
echo "### GET /mcp (no server-sent events stream)"; echo; echo '```http'; curl -s -o /dev/null -w 'HTTP %{http_code}\n' "$GW"; echo '```'; echo
echo "### Inside sdk-lead, through the door: curl is refused"; echo; echo '```http'
$OS sandbox exec sdk-lead -- curl -s -m 10 -w '\nHTTP %{http_code}\n' -X POST http://openshell.local/mcp -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"ping"}' 2>&1 | tail -2
echo '```'; echo
echo "### Inside sdk-lead, through the door: node is allowed (tools/list)"; echo; echo '```http'
$OS sandbox exec sdk-lead -- node -e 'fetch("http://openshell.local/mcp",{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({jsonrpc:"2.0",id:1,method:"tools/list"})}).then(async r=>{console.log("HTTP "+r.status);const j=await r.json();console.log(JSON.stringify(j.result.tools.map(t=>t.name)))})' 2>&1 | tail -2
echo '```'
rm -f /tmp/mcp-body.$$
