#!/usr/bin/env bash
# Checks for mark_done and wait(for_done) against a running dev gateway:
# fan-out, grandchildren, stop and the program check. No model needed; children
# write /tmp/result.txt and call mark_done with the scripted MCP client. The
# gateway keeps only the "done" flag; the parent reads the file with get_file.
# Needs: dev gateway (mise run gateway:docker) and image pi-agent:local.
set -u
cd "$(dirname "$0")"
OS="${OPENSHELL_BIN:-../../target/debug/openshell} -g ${GATEWAY:-docker-dev}"
CLIENT=/usr/local/lib/gateway-mcp/mcp-client.mjs
mcp() { $OS sandbox exec "$1" -- node "$CLIENT" "${@:2}"; }
wait_ready() { for _ in $(seq 1 60); do $OS sandbox get "$1" 2>/dev/null | grep -q 'Phase: Ready' && return 0; sleep 2; done; return 1; }
wait_gone() { for _ in $(seq 1 60); do $OS sandbox list 2>/dev/null | grep -q "^$1 " || return 0; sleep 2; done; return 1; }
# A child that writes RESULT to /tmp/result.txt after DELAY seconds, marks
# itself done, then runs THEN (sleep to stay readable, or exit).
finishes() { # name delay result then
  local cmd="sleep $2; echo $3 > /tmp/result.tmp && mv /tmp/result.tmp /tmp/result.txt; node $CLIENT mark_done '{}'; $4"
  python3 -c 'import json,sys; print(json.dumps({"name":sys.argv[1],"command":["sh","-c",sys.argv[2]]}))' "$1" "$cmd"
}
result() { mcp "$1" get_file "{\"handle\":\"$2\",\"path\":\"/tmp/result.txt\"}"; }

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
mcp lead create_sandbox "$(finishes a 20 result-a 'sleep 600')" >/dev/null
mcp lead create_sandbox "$(finishes b 5 result-b 'sleep 600')" >/dev/null
mcp lead create_sandbox "$(finishes c 12 result-c 'exit 0')" >/dev/null
tmp=$(mktemp -d)
for h in a b c; do
  ( start=$(date +%s); out=$(mcp lead wait "{\"handle\":\"$h\",\"timeout_s\":180,\"for_done\":true}")
    echo "$out $(( $(date +%s) - start ))s" > "$tmp/$h" ) &
done
wait
for h in a b c; do check "wait $h returns done" "$(cat "$tmp/$h")" '"done":true'; done
echo "     wait times: a $(grep -o '[0-9]*s$' "$tmp/a"), b $(grep -o '[0-9]*s$' "$tmp/b"), c $(grep -o '[0-9]*s$' "$tmp/c")"
check "the lead reads a's result" "$(result lead a)" 'result-a'
check "the lead reads b's result" "$(result lead b)" 'result-b'
check "c exited after done, so its file is gone" "$(result lead c)" 'isError":true'

echo "2. Grandchild: lead -> w1 -> g1"
mcp lead create_sandbox '{"name":"w1","command":["sleep","3600"]}' >/dev/null
mcp lead wait '{"handle":"w1","timeout_s":120}' >/dev/null
out=$(mcp w1 create_sandbox "$(finishes g1 3 from-g1 'sleep 600')")
check "w1 may create g1 (depth 2)" "$out" '"handle":"g1"'
check "w1 waits for g1 to be done" "$(mcp w1 wait '{"handle":"g1","timeout_s":120,"for_done":true}')" '"done":true'
check "w1 reads g1's result" "$(result w1 g1)" 'from-g1'
check "the lead also sees g1 done (a flag, no content)" \
  "$(mcp lead wait '{"handle":"g1","timeout_s":10,"for_done":true}')" '"done":true'
check "w1 is not done" "$(mcp lead wait '{"handle":"w1","timeout_s":3,"for_done":true}')" '"done":false'

echo "3. Stop: a stopped child takes its unread result with it"
mcp lead create_sandbox "$(finishes s1 3 unread 'sleep 600')" >/dev/null
check "s1 is done" "$(mcp lead wait '{"handle":"s1","timeout_s":120,"for_done":true}')" '"done":true'
check "stop s1" "$(mcp lead stop_sandbox '{"handle":"s1"}')" '"stopped":true'
wait_gone s1
check "s1 is gone for the lead" "$(mcp lead wait '{"handle":"s1","timeout_s":3,"for_done":true}')" 'NOT_YOUR_CHILD'

echo "4. Who may mark done"
check "the lead has no parent to tell" "$(mcp lead mark_done '{}')" 'NOT_A_CHILD'
body='{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"mark_done","arguments":{}}}'
out=$($OS sandbox exec w1 -- curl -s -X POST http://openshell.local/mcp -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' -d "$body")
check "curl in w1 is refused (grant allows node only)" "$out" 'PROGRAM_NOT_ALLOWED'
check "w1 is still not done" "$(mcp lead wait '{"handle":"w1","timeout_s":3,"for_done":true}')" '"done":false'
check "w1 marks done" "$(mcp w1 mark_done '{}')" '"repeated":false'
check "again is a no-op" "$(mcp w1 mark_done '{}')" '"repeated":true'
check "the lead sees w1 done" "$(mcp lead wait '{"handle":"w1","timeout_s":3,"for_done":true}')" '"done":true'

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
