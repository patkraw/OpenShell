#!/usr/bin/env bash
# Checks for wait(until_file) against a running dev gateway: fan-out,
# grandchildren, stop and a child that exits early. No model needed; children
# write /tmp/result.txt and never call the gateway. The gateway checks for the
# file through the exec relay and stores nothing; the parent reads it with get_file.
# Needs: dev gateway (mise run gateway:docker) and image pi-agent:local.
set -u
cd "$(dirname "$0")"
OS="${OPENSHELL_BIN:-../../target/debug/openshell} -g ${GATEWAY:-docker-dev}"
CLIENT=/usr/local/lib/gateway-mcp/mcp-client.mjs
mcp() { $OS sandbox exec "$1" -- node "$CLIENT" "${@:2}"; }
wait_ready() { for _ in $(seq 1 60); do $OS sandbox get "$1" 2>/dev/null | grep -q 'Phase: Ready' && return 0; sleep 2; done; return 1; }
wait_gone() { for _ in $(seq 1 60); do $OS sandbox list 2>/dev/null | grep -q "^$1 " || return 0; sleep 2; done; return 1; }
# A child that writes RESULT to /tmp/result.txt after DELAY seconds (whole,
# then renamed), then runs THEN (sleep to stay readable, or exit).
finishes() { # name delay result then
  local cmd="sleep $2; echo $3 > /tmp/result.tmp && mv /tmp/result.tmp /tmp/result.txt; $4"
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
mcp lead create_sandbox "$(finishes c 12 result-c 'sleep 600')" >/dev/null
tmp=$(mktemp -d)
for h in a b c; do
  ( start=$(date +%s); out=$(mcp lead wait "{\"handle\":\"$h\",\"timeout_s\":180,\"until_file\":\"/tmp/result.txt\"}")
    echo "$out $(( $(date +%s) - start ))s" > "$tmp/$h" ) &
done
wait
for h in a b c; do check "wait $h returns when the file exists" "$(cat "$tmp/$h")" '"file_ready":true'; done
echo "     wait times: a $(grep -o '[0-9]*s$' "$tmp/a"), b $(grep -o '[0-9]*s$' "$tmp/b"), c $(grep -o '[0-9]*s$' "$tmp/c")"
for h in a b c; do check "the lead reads $h's result" "$(result lead $h)" "result-$h"; done

echo "2. Grandchild: lead -> w1 -> g1"
mcp lead create_sandbox '{"name":"w1","command":["sleep","3600"]}' >/dev/null
mcp lead wait '{"handle":"w1","timeout_s":120}' >/dev/null
out=$(mcp w1 create_sandbox "$(finishes g1 3 from-g1 'sleep 600')")
check "w1 may create g1 (depth 2)" "$out" '"handle":"g1"'
check "w1 waits for g1's file" "$(mcp w1 wait '{"handle":"g1","timeout_s":120,"until_file":"/tmp/result.txt"}')" '"file_ready":true'
check "w1 reads g1's result" "$(result w1 g1)" 'from-g1'
check "w1 has no result file yet" "$(mcp lead wait '{"handle":"w1","timeout_s":5,"until_file":"/tmp/result.txt"}')" '"timed_out":true'

echo "3. A child that exits without writing its file"
mcp lead create_sandbox '{"name":"x1","command":["sh","-c","sleep 3; exit 7"]}' >/dev/null
out=$(mcp lead wait '{"handle":"x1","timeout_s":120,"until_file":"/tmp/result.txt"}')
check "wait returns at exit" "$out" '"file_ready":false'
check "with the exit code" "$out" '"exit_code":7'

echo "4. Stop: a stopped child takes its unread result with it"
mcp lead create_sandbox "$(finishes s1 3 unread 'sleep 600')" >/dev/null
check "s1's file is ready" "$(mcp lead wait '{"handle":"s1","timeout_s":120,"until_file":"/tmp/result.txt"}')" '"file_ready":true'
check "stop s1" "$(mcp lead stop_sandbox '{"handle":"s1"}')" '"stopped":true'
wait_gone s1
check "s1 is gone for the lead" "$(mcp lead wait '{"handle":"s1","timeout_s":3,"until_file":"/tmp/result.txt"}')" 'NOT_YOUR_CHILD'

echo "5. Arguments"
check "a relative path is refused" "$(mcp lead wait '{"handle":"w1","timeout_s":3,"until_file":"result.txt"}')" 'INVALID_ARGUMENT'
check "mark_done no longer exists" "$(mcp w1 mark_done '{}')" 'isError":true'

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
