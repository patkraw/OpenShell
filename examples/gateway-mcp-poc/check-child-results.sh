#!/usr/bin/env bash
# Checks for submit_result and wait(for_result) against a running dev gateway:
# fan-out, grandchildren, stop, the program check and the limits. No model needed;
# children submit with the scripted MCP client.
# Needs: dev gateway (mise run gateway:docker) and image pi-agent:local.
set -u
cd "$(dirname "$0")"
OS="${OPENSHELL_BIN:-../../target/debug/openshell} -g ${GATEWAY:-docker-dev}"
CLIENT=/usr/local/lib/gateway-mcp/mcp-client.mjs
mcp() { $OS sandbox exec "$1" -- node "$CLIENT" "${@:2}"; }
wait_ready() { for _ in $(seq 1 60); do $OS sandbox get "$1" 2>/dev/null | grep -q 'Phase: Ready' && return 0; sleep 2; done; return 1; }
wait_gone() { for _ in $(seq 1 60); do $OS sandbox list 2>/dev/null | grep -q "^$1 " || return 0; sleep 2; done; return 1; }
# A child command that submits RESULT after DELAY seconds, then sleeps or exits.
submits() { # name delay result then
  local cmd="sleep $2; node $CLIENT submit_result '{\"result\":\"$3\"}'; $4"
  python3 -c 'import json,sys; print(json.dumps({"name":sys.argv[1],"command":["sh","-c",sys.argv[2]]}))' "$1" "$cmd"
}

pass=0 fail=0
check() { # label actual expected-substring
  if [[ "$2" == *"$3"* ]]; then pass=$((pass + 1)); echo "  ok   $1"
  else fail=$((fail + 1)); echo "  FAIL $1"; echo "       want: $3"; echo "       got:  $2"; fi
}

$OS sandbox delete lead >/dev/null 2>&1; wait_gone lead
$OS sandbox create --name lead --from pi-agent:local --policy lead.yaml \
  --grant-program /usr/local/bin/node \
  --grant-max-depth 2 --grant-max-breadth 6 --grant-max-total 10 --grant-expires 1h \
  --no-tty --detach -- sleep 3600 >/dev/null
wait_ready lead || { echo "lead not ready"; exit 1; }

echo "1. Fan-out: three children, three waits at once"
mcp lead create_sandbox "$(submits a 20 result-a 'sleep 600')" >/dev/null
mcp lead create_sandbox "$(submits b 5 result-b 'sleep 600')" >/dev/null
mcp lead create_sandbox "$(submits c 12 result-c 'exit 0')" >/dev/null
tmp=$(mktemp -d)
for h in a b c; do
  ( start=$(date +%s); out=$(mcp lead wait "{\"handle\":\"$h\",\"timeout_s\":180,\"for_result\":true}")
    echo "$out $(( $(date +%s) - start ))s" > "$tmp/$h" ) &
done
wait
for h in a b c; do check "wait $h returns result-$h" "$(cat "$tmp/$h")" "\"result\":\"result-$h\""; done
echo "     wait times: a $(grep -o '[0-9]*s$' "$tmp/a"), b $(grep -o '[0-9]*s$' "$tmp/b"), c $(grep -o '[0-9]*s$' "$tmp/c")"

echo "2. Grandchild: lead -> w1 -> g1"
mcp lead create_sandbox '{"name":"w1","command":["sleep","3600"]}' >/dev/null
mcp lead wait '{"handle":"w1","timeout_s":120}' >/dev/null
out=$(mcp w1 create_sandbox "$(submits g1 3 from-g1 'sleep 600')")
check "w1 may create g1 (depth 2)" "$out" '"handle":"g1"'
check "w1 waits for g1's result" "$(mcp w1 wait '{"handle":"g1","timeout_s":120,"for_result":true}')" '"result":"from-g1"'
check "the lead may not wait for g1's result" \
  "$(mcp lead wait '{"handle":"g1","timeout_s":10,"for_result":true}')" 'NOT_DIRECT_CHILD'
check "a plain wait on g1 shows no result to the lead" \
  "$(mcp lead wait '{"handle":"g1","timeout_s":10}')" '"result":null' 
check "w1 has submitted nothing, so the lead sees no result" \
  "$(mcp lead wait '{"handle":"w1","timeout_s":3,"for_result":true}')" '"result":null'

echo "3. Stop: a stopped child takes its unread result with it"
mcp lead create_sandbox "$(submits s1 3 unread 'sleep 600')" >/dev/null
check "s1's result is saved" "$(mcp lead wait '{"handle":"s1","timeout_s":120,"for_result":true}')" '"result":"unread"'
check "stop s1" "$(mcp lead stop_sandbox '{"handle":"s1"}')" '"stopped":true'
wait_gone s1
check "s1 is gone for the lead" "$(mcp lead wait '{"handle":"s1","timeout_s":3,"for_result":true}')" 'NOT_YOUR_CHILD'

echo "4. Program check: the grant allows node only"
body='{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"submit_result","arguments":{"result":"from curl"}}}'
out=$($OS sandbox exec w1 -- curl -s -X POST http://openshell.local/mcp -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' -d "$body")
check "curl in w1 is refused" "$out" 'PROGRAM_NOT_ALLOWED'
check "w1 still has no result" "$(mcp lead wait '{"handle":"w1","timeout_s":3,"for_result":true}')" '"result":null'

echo "5. Limits"
# A JS one-liner in w1 (node, so the program check passes) that submits N bytes.
submit_bytes() {
  $OS sandbox exec w1 -- node -e "
    const b = JSON.stringify({jsonrpc: '2.0', id: 1, method: 'tools/call',
      params: {name: 'submit_result', arguments: {result: 'x'.repeat($1)}}});
    fetch('http://openshell.local/mcp', {method: 'POST', body: b,
      headers: {'content-type': 'application/json', accept: 'application/json, text/event-stream'}})
      .then(async (r) => console.log(r.status, await r.text()),
            (e) => console.log('connection closed: ' + (e.cause?.code ?? e.message)));"
}
check "33 KiB is refused by the gateway" "$(submit_bytes $((33 * 1024)))" 'TOO_LARGE'
check "70 KiB is refused by the door, with a reason" "$(submit_bytes $((70 * 1024)))" 'request_too_large'
check "w1 still has no result" "$(mcp lead wait '{"handle":"w1","timeout_s":3,"for_result":true}')" '"result":null'
check "w1 submits" "$(mcp w1 submit_result '{"result":"first"}')" '"saved":true'
check "same text again is a no-op" "$(mcp w1 submit_result '{"result":"first"}')" '"repeated":true'
check "different text is refused" "$(mcp w1 submit_result '{"result":"second"}')" 'ALREADY_SUBMITTED'
check "the lead gets the first" "$(mcp lead wait '{"handle":"w1","timeout_s":3,"for_result":true}')" '"result":"first"'

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
