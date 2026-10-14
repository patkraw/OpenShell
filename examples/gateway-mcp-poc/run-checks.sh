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
check "the lead's policy has no door rule (the door is open in every sandbox)" "$($OS sandbox get lead --policy-only 2>&1 | grep -c openshell.local)" "0"
check "lead sees create_sandbox" "$(mcp lead tools/list)" "create_sandbox"
check "curl in the lead is refused by the gateway (not the grant's program)" \
  "$($OS sandbox exec lead -- curl -s -m 10 -X POST http://openshell.local/mcp -d '{}' 2>&1 | tail -1)" "PROGRAM_NOT_ALLOWED"

echo "== Creating children"
W1='{"name":"w1","command":["sleep","600"],"request_id":"r1","policy":{"network_policies":{"example":{"name":"example","endpoints":[{"host":"example.com","port":443}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
out=$(mcp lead create_sandbox "$W1"); check "create w1 inside the lead's policy" "$out" '"handle":"w1"'
check "retry with same request_id returns the first result" "$(mcp lead create_sandbox "$W1")" '"repeated_request":true'
check "retry counted once (total 2 of 3)" "$(mcp lead get_limits)" '"total":2'
WIDE='{"name":"wide","command":["sleep","60"],"policy":{"network_policies":{"gh":{"name":"gh","endpoints":[{"host":"api.github.com","port":443,"protocol":"rest","enforcement":"enforce","rules":[{"allow":{"method":"POST","path":"/repos/**"}}]}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
check "child wider than the lead's policy is refused" "$(mcp lead create_sandbox "$WIDE")" "EXCEEDS_CEILING"
MCPR='{"name":"m","command":["sleep","60"],"policy":{"network_policies":{"gh":{"name":"gh","endpoints":[{"host":"api.githubcopilot.com","port":443,"path":"/mcp","protocol":"mcp","enforcement":"enforce","rules":[{"allow":{"method":"tools/call","tool":"get_issue"}}]}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
check "child policy with MCP rules is refused" "$(mcp lead create_sandbox "$MCPR")" "UNSUPPORTED_RULES"
wait_ready w1
check "w1 runs the lead's image" "$(docker inspect --format '{{.Image}}' $(docker ps -q --filter name=--w1-))" "$(docker image inspect --format '{{.Id}}' pi-agent:local)"

echo "== Worker w1 (depth 1 = max)"
check "w1 reaches the door and sees create_sandbox (tools are always listed)" "$(mcp w1 tools/list)" "create_sandbox"
check "w1 is refused: no depth left" "$(mcp w1 create_sandbox '{"name":"g1","command":["sleep","60"]}')" "LIMIT_REACHED"
check "w1's limits say it cannot create children now" "$(mcp w1 get_limits)" '"can_create_children":false'
check "lead reads a file from w1 (get_file)" "$(mcp lead get_file '{"handle":"w1","path":"/etc/hostname"}')" '"content":'
check "lead cannot read files of a non-child" "$(mcp lead get_file '{"handle":"lead","path":"/etc/hostname"}')" "NOT_YOUR_CHILD"

echo "== Limits"
W2='{"name":"w2","command":["sleep","600"]}'
check "create w2 (breadth 2 of 2)" "$(mcp lead create_sandbox "$W2")" '"handle":"w2"'
check "create_sandbox still listed with the budget used" "$(mcp lead tools/list)" "create_sandbox"
out=$(mcp lead create_sandbox '{"name":"w3","command":["sleep","60"]}')
if [[ "$out" == *NOT_ALLOWED* || "$out" == *LIMIT_REACHED* ]]; then check "third child refused" "ok" "ok"; else check "third child refused" "$out" "refused"; fi

echo "== Grant changes"
check "a sandbox cannot change grants" "$(mcp lead grant_update '{"grant_id":"x","max_total":9}')" "NOT_ALLOWED"
gid=$(mcp lead get_limits | python3 -c 'import json,sys; print(json.load(sys.stdin)["grant_id"])')
check "the user widens the grant (depth 2, breadth 9, total 10)" "$(user_mcp grant_update grant_id="$gid" max_depth=2 max_breadth=9 max_total=10)" '"max_depth": 2'
check "lead can create again after widening" "$(mcp lead create_sandbox '{"name":"w3","command":["sh","-c","echo result-from-w3; exit 0"],"lifetime":"until_exit"}')" '"handle":"w3"'
check "w1, a leaf before, creates a child at once (no policy change)" "$(mcp w1 create_sandbox '{"name":"g1","command":["sleep","60"]}')" '"handle":"g1"'
check "wait for w3 to exit" "$(mcp lead wait '{"handle":"w3","timeout_s":60}')" '"exit_code":0'
check "only your own children" "$(mcp lead stop_sandbox '{"handle":"lead"}')" "NOT_YOUR_CHILD"

echo "== Tool sandboxes (kind: tool)"
check "create tool sandbox t1 (no command needed)" "$(mcp lead create_sandbox '{"name":"t1","kind":"tool"}')" '"kind":"tool"'
mcp lead create_sandbox '{"name":"t0","kind":"tool"}' >/dev/null; wait_ready t0
check "tool sandboxes can never create children" "$(mcp t0 create_sandbox '{"name":"t00","command":["true"]}')" "tool sandboxes cannot create children"
wait_ready t1
check "exec runs a command in the tool sandbox" "$(mcp lead exec '{"handle":"t1","command":["sh","-c","echo out-line; echo err-line >&2; exit 3"]}')" '"exit_code":3'
check "exec returns stdout" "$(mcp lead exec '{"handle":"t1","command":["sh","-c","echo out-line"]}')" 'out-line'
check "exec on an agent child is refused" "$(mcp lead exec '{"handle":"w1","command":["id"]}')" "WRONG_KIND"
check "bad kind is refused" "$(mcp lead create_sandbox '{"name":"x","kind":"robot","command":["true"]}')" "INVALID_ARGUMENT"
check "short idle timeout accepted" "$(mcp lead create_sandbox '{"name":"t2","kind":"tool","idle_timeout_s":10}')" '"idle_timeout_s":10'
sleep 25
check "idle tool sandbox t2 stopped automatically" "[$($OS sandbox list 2>&1 | grep -E '^t2 ' | grep -v -i deleting || true)]" "[]"

echo "== The user on the host, through MCP"
check "the user sees create_sandbox and grant tools, not child tools" "$(./user-mcp.py grant_list >/dev/null; python3 - <<'PY'
import json, urllib.request
req = urllib.request.Request("http://127.0.0.1:18080/mcp", json.dumps({"jsonrpc":"2.0","id":1,"method":"tools/list"}).encode(), {"content-type":"application/json"})
print([t["name"] for t in json.load(urllib.request.urlopen(req))["result"]["tools"]])
PY
)" "['create_sandbox', 'grant_list', 'grant_update']"
$OS sandbox delete hostlead >/dev/null 2>&1; wait_gone hostlead
check "the user creates a top-level sandbox with a grant via MCP" "$(user_mcp create_sandbox name=hostlead image=pi-agent:local 'command=["sleep","3600"]' grant=max_depth=1,max_breadth=2,max_total=3,expires=1h,program=/usr/local/bin/node,child_policy=inherit)" '"has_grant": true'
wait_ready hostlead
check "that sandbox can create children" "$(mcp hostlead tools/list)" "create_sandbox"
check "child_policy=inherit: a child policy is refused" "$(mcp hostlead create_sandbox '{"name":"hc1","command":["sleep","60"],"policy":{"network_policies":{}}}')" "omit policy"
check "child_policy=inherit: a child without policy is created" "$(mcp hostlead create_sandbox '{"name":"hc1","command":["sleep","60"]}')" '"handle":"hc1"'
check "that child reaches Ready (explicit default policy, no image policy)" "$(wait_ready hc1 && echo ready)" "ready"
$OS sandbox delete hostlead >/dev/null 2>&1

echo "== Each level can only narrow (child proved against its parent's policy)"
$OS sandbox delete deeplead >/dev/null 2>&1; wait_gone deeplead
$OS sandbox create --name deeplead --from pi-agent:local --policy lead.yaml \
  --grant max_depth=2,max_breadth=2,max_total=4,expires=1h,program=/usr/local/bin/node \
  --no-tty --detach -- sleep 3600 >/dev/null 2>&1
wait_ready deeplead || echo "deeplead did not become ready"
GH_ONLY='{"name":"mid","command":["sleep","3600"],"policy":{"network_policies":{"github_read":{"name":"github_read","endpoints":[{"host":"api.github.com","port":443,"protocol":"rest","enforcement":"enforce","rules":[{"allow":{"method":"GET","path":"/repos/**"}}]}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
check "deeplead creates mid with GitHub read only" "$(mcp deeplead create_sandbox "$GH_ONLY")" '"handle":"mid"'
wait_ready mid || echo "mid did not become ready"
EX='{"name":"leaf","command":["true"],"policy":{"network_policies":{"example":{"name":"example","endpoints":[{"host":"example.com","port":443}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
check "mid cannot give a grandchild example.com (deeplead has it, mid does not)" "$(mcp mid create_sandbox "$EX")" "EXCEEDS_CEILING"
check "mid can give a grandchild what mid has" "$(mcp mid create_sandbox "${GH_ONLY/\"mid\"/\"leaf\"}")" '"handle":"leaf"'
$OS sandbox delete deeplead >/dev/null 2>&1

echo "== Cascade stop"
$OS sandbox delete lead >/dev/null 2>&1; sleep 5
left=$($OS sandbox list 2>&1 | grep -E '^(w1|w2|w3|g1) ' | grep -v -i deleting || true)
check "deleting the lead removed w1, w2, w3 and grandchild g1" "[$left]" "[]"

echo; echo "$pass passed, $fail failed"
