# 02 · MCP inside a root

**Goal:** play the agent in a root: call every MCP tool from inside `lead`,
retry a create safely, and see what the gateway refuses.

Setup: [01](01-cli-children.md) (`lead` and `w1` running), plus the shell
setup from the [index](README.md).

Inside every sandbox, `http://openshell.local/mcp` is the gateway's MCP
endpoint. The sandbox's supervisor adds the sandbox's identity to each call,
so a root's agent needs no token and can only see and manage its own
children.

## 1. The tools

```shell
openshell sandbox exec --name lead --no-tty -- curl -s -X POST http://openshell.local/mcp \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | jq -r '.result.tools[].name'
```

```text
create_sandbox
list
exec
wait
delete
logs
```

The description of each tool tells an agent how to use it; read them with
`jq '.result.tools'` instead. When templates are published for agents, the
`create_sandbox` description also names them ([04](04-templates.md)).

## 2. Create a child, and retry the create

```shell
mcp lead create_sandbox '{"name":"a1"}'
```

```json
{"already_existed":false,"created_at":"2026-11-05T02:44:12.638Z","id":"36c543da-7887-4ea5-9bb8-37ac2f3b9005","name":"a1","parent":"lead","state":"ready"}
```

The reply comes once the child is ready. An agent that lost the reply can
send the same call again and gets the same child back:

```shell
mcp lead create_sandbox '{"name":"a1"}'
```

```json
{"already_existed":true,"created_at":"2026-11-05T02:44:12.638Z","id":"36c543da-7887-4ea5-9bb8-37ac2f3b9005","name":"a1","parent":"lead","state":"ready"}
```

A child an agent creates gets its parent's image (here `child-base:local`)
unless the call names a `template` ([04](04-templates.md)).

## 3. List the children

```shell
mcp lead list | jq -c '[.sandboxes[] | {name, parent, state}]'
```

```json
[{"name":"w1","parent":"lead","state":"ready"},{"name":"a1","parent":"lead","state":"ready"}]
```

The children you made with the CLI show up too: they have the same parent.

## 4. Run a command

```shell
mcp lead exec '{"name":"a1","command":["sh","-c","echo hello from $(hostname); head -1 /etc/os-release"]}'
```

```json
{"binary_output":false,"exit_code":0,"stderr":"","stderr_truncated":false,"stdout":"hello from 309d5d95c3e9\nPRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\n","stdout_truncated":false,"timed_out":false}
```

`command` is an argument list, not a shell line; use `sh -c` for pipes and
variables. `stdin` sends text (`stdin_base64` sends bytes) and `workdir`
sets the directory:

```shell
mcp lead exec '{"name":"a1","command":["wc","-c"],"stdin":"twelve bytes"}' | jq -r .stdout
```

```text
12
```

`exec` waits up to `timeout_s` seconds (default 30, at most 45). When the
time runs out the command may still be running:

```shell
mcp lead exec '{"name":"a1","command":["sleep","10"],"timeout_s":2}' | jq -c '{exit_code, timed_out, message}'
```

```json
{"exit_code":null,"timed_out":true,"message":"the command may still be running in the child; for long work use background: true and wait, or delete the child to stop everything"}
```

Longer work belongs in a background job: [03](03-long-work.md).

## 5. Read a child's logs

```shell
mcp lead logs '{"name":"a1","lines":3}'
```

```json
{"lines":[{"level":"OCSF","message":"EVENT [INFO] ssh relay open (channel_id=d535ac37-…, target=unix:/run/openshell/ssh.sock)","time":"2026-11-05T02:44:24.810Z"},{"level":"OCSF","message":"SSH:OPEN [INFO] ALLOWED","time":"2026-11-05T02:44:24.810Z"},{"level":"OCSF","message":"SSH:FAIL [LOW]","time":"2026-11-05T02:44:26.815Z"}],"truncated":false}
```

`lines` goes up to 2000; `since` takes an RFC 3339 time.

## 6. What the gateway refuses

Errors come back as `{"error", "message", "retryable"}`.

Any sandbox that is not the caller's child is `not_found`, even when it
exists, and even when it is the caller itself:

```shell
mcp lead exec '{"name":"lead","command":["true"]}'
```

```json
{"error":"not_found","message":"sandbox 'lead' not found","retryable":false}
```

Settings that only users may choose:

```shell
mcp lead create_sandbox '{"name":"a2","cpu":"2"}'
mcp lead create_sandbox '{"name":"a2","image":"alpine"}'
```

```json
{"error":"invalid_argument","message":"'cpu' is not available over MCP; users can set it with the CLI","retryable":false}
{"error":"invalid_argument","message":"'image' is only available to users","retryable":false}
```

A provider the root does not have, and a template not published for agents:

```shell
mcp lead create_sandbox '{"name":"a2","providers":["my-github"]}'
mcp lead create_sandbox '{"name":"a2","template":"nope"}'
```

```json
{"error":"failed_precondition","message":"provider 'my-github' is not attached to parent 'lead'","retryable":false}
{"error":"failed_precondition","message":"template 'nope' is not available to agents","retryable":false}
```

Other refusals you may meet:

- `limit_reached`: `sandbox 'lead' has reached its limit of 10 children`.
  Children still being deleted count, and the message says so; try again
  shortly.
- `rate_limited` (retryable): too many calls; slow down and retry.
- `internal`: `internal error (request <id>); the operator can find it in the
  gateway logs`. The detail stays in the gateway logs.

## 7. Delete a child

Make a throwaway child and delete it:

```shell
mcp lead create_sandbox '{"name":"tmp1"}' > /dev/null
mcp lead delete '{"name":"tmp1"}'
```

```json
{"deleting":true,"name":"tmp1"}
```

Deleting stops everything running in the child. A second delete of the same
name is `not_found`.

## Clean up

Keep `lead`, `w1` and `a1` for 03 and 05. Otherwise delete the children, then
`lead`.

## What you learned

- An agent in a root reaches the tools at `http://openshell.local/mcp`, with
  no token.
- A root sees only its own children; everything else is `not_found`.
- `create_sandbox` is safe to retry, and an agent's child gets its parent's
  image.
- `exec` is for short commands; it stops waiting after 45 seconds.
