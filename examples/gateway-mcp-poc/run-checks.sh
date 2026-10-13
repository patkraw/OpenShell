#!/usr/bin/env bash
# Gateway MCP POC checks against a dev gateway (mise run gateway:docker).
# Starts a lead with a grant, then exercises every rule from the RFC decisions.
set -u
cd "$(dirname "$0")"
OS="${OPENSHELL_BIN:-../../target/debug/openshell} -g ${GATEWAY:-docker-dev}"
GW_URL="${GW_URL:-http://127.0.0.1:18080/mcp}"
pass=0; fail=0
mcp() { local sb=$1; shift; $OS sandbox exec "$sb" -- node /usr/local/lib/gateway-mcp/mcp-client.mjs "$@" 2>&1 | tail -1; }
check() { # name, output, expected substring
  if [[ "$2" == *"$3"* ]]; then echo "PASS  $1"; pass=$((pass+1)); else echo "FAIL  $1"; echo "      got: $2"; fail=$((fail+1)); fi; }
user_mcp() { ./user-mcp.py "$@"; }

wait_ready() { for _ in $(seq 1 60); do $OS sandbox get "$1" 2>/dev/null | grep -q 'Phase: Ready' && return 0; sleep 2; done; return 1; }
wait_gone() { for _ in $(seq 1 60); do $OS sandbox list 2>/dev/null | grep -q "^$1 " || return 0; sleep 2; done; }
$OS sandbox delete lead >/dev/null 2>&1; wait_gone lead
$OS sandbox create --name lead --from pi-agent:local --policy lead.yaml \
  --grant max_depth=1,max_breadth=2,max_total=3,expires=4h,program=/usr/local/bin/node \
  --no-tty --detach -- sleep 3600 >/dev/null 2>&1
wait_ready lead || { echo "lead did not become ready"; exit 1; }

echo "== Lead (started with --grant)"
check "gateway wrote the door rule for node" "$($OS sandbox get lead --policy-only 2>&1)" "openshell_gateway_mcp"
check "lead sees create_sandbox" "$(mcp lead tools/list)" "create_sandbox"
check "curl in the lead is refused by the door (wrong program)" \
  "$($OS sandbox exec lead -- curl -s -m 10 -X POST http://openshell.local/mcp -d '{}' 2>&1 | tail -1)" "door_denied"

echo "== Creating children"
W1='{"name":"w1","command":["sleep","600"],"request_id":"r1","policy":{"network_policies":{"example":{"name":"example","endpoints":[{"host":"example.com","port":443}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
out=$(mcp lead create_sandbox "$W1"); check "create w1 inside the ceiling" "$out" '"handle":"w1"'
check "retry with same request_id returns the first result" "$(mcp lead create_sandbox "$W1")" '"repeated_request":true'
check "retry counted once (total 2 of 3)" "$(mcp lead get_limits)" '"total":2'
WIDE='{"name":"wide","command":["sleep","60"],"policy":{"network_policies":{"gh":{"name":"gh","endpoints":[{"host":"api.github.com","port":443,"protocol":"rest","enforcement":"enforce","rules":[{"allow":{"method":"POST","path":"/repos/**"}}]}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
check "child wider than the ceiling is refused" "$(mcp lead create_sandbox "$WIDE")" "EXCEEDS_CEILING"
MCPR='{"name":"m","command":["sleep","60"],"policy":{"network_policies":{"gh":{"name":"gh","endpoints":[{"host":"api.githubcopilot.com","port":443,"path":"/mcp","protocol":"mcp","enforcement":"enforce","rules":[{"allow":{"method":"tools/call","tool":"get_issue"}}]}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
check "child policy with MCP rules is refused" "$(mcp lead create_sandbox "$MCPR")" "UNSUPPORTED_RULES"
DOOR='{"name":"d","command":["sleep","60"],"policy":{"network_policies":{"openshell_gateway_mcp":{"name":"openshell_gateway_mcp","endpoints":[{"host":"openshell.local","port":80}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
check "agent cannot write the door rule itself" "$(mcp lead create_sandbox "$DOOR")" "door rule is written by the gateway"
wait_ready w1
check "w1 runs the lead's image" "$(docker inspect --format '{{.Image}}' $(docker ps -q --filter name=--w1-))" "$(docker image inspect --format '{{.Id}}' pi-agent:local)"

echo "== Worker w1 (depth 1 = max)"
check "w1 has no door rule" "$($OS sandbox get w1 --policy-only 2>&1 | grep -c openshell_gateway_mcp)" "0"
check "w1 cannot reach the door at all" "$(mcp w1 tools/list)" "door_denied"
check "lead reads a file from w1 (get_file)" "$(mcp lead get_file '{"handle":"w1","path":"/etc/hostname"}')" '"content":'
check "lead cannot read files of a non-child" "$(mcp lead get_file '{"handle":"lead","path":"/etc/hostname"}')" "NOT_YOUR_CHILD"

echo "== Limits"
W2='{"name":"w2","command":["sleep","600"]}'
check "create w2 (breadth 2 of 2)" "$(mcp lead create_sandbox "$W2")" '"handle":"w2"'
check "create_sandbox now hidden from the lead (budget used)" "$(mcp lead tools/list)" '["wait"'
check "third child refused" "$(mcp lead create_sandbox '{"name":"w3","command":["sleep","60"]}')" "LIMIT_REACHED"

echo "== Grant changes"
check "a sandbox cannot change grants" "$(mcp lead grant_update '{"grant_id":"x","max_total":9}')" "NOT_ALLOWED"
gid=$(mcp lead get_limits | python3 -c 'import json,sys; print(json.load(sys.stdin)["grant_id"])')
check "the user widens the grant (breadth 3, total 4)" "$(user_mcp grant_update grant_id="$gid" max_breadth=3 max_total=4)" '"max_breadth": 3'
check "lead can create again after widening" "$(mcp lead create_sandbox '{"name":"w3","command":["sh","-c","echo result-from-w3; exit 0"],"lifetime":"until_exit"}')" '"handle":"w3"'
check "wait for w3 to exit" "$(mcp lead wait '{"handle":"w3","timeout_s":60}')" '"exit_code":0'
check "only your own children" "$(mcp lead stop_sandbox '{"handle":"lead"}')" "NOT_YOUR_CHILD"

echo "== Cascade stop"
$OS sandbox delete lead >/dev/null 2>&1; sleep 5
left=$($OS sandbox list 2>&1 | grep -E '^(w1|w2|w3) ' | grep -v -i deleting || true)
check "deleting the lead removed w1, w2, w3" "[$left]" "[]"

echo; echo "$pass passed, $fail failed"
