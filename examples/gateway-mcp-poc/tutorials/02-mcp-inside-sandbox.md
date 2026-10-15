# 02 · MCP inside a sandbox

**Goal:** call every gateway MCP tool from **inside** a sandbox, the way an
agent does, and see why each refusal happens.

Setup: [00](00-setup.md), plus the shell setup from the [index](README.md).

## How it works

- Inside every sandbox the gateway MCP is at **`http://openshell.local/mcp`**.
- The sandbox's own **supervisor** answers that address and forwards the call
  to the gateway with the sandbox's identity and the **calling program**.
- The agent holds **no credential**. The gateway decides every call.

Start a lead:

```shell
openshell sandbox create --name lead --from pi-agent:local \
  --policy examples/gateway-mcp-poc/lead.yaml \
  --grant-program /usr/local/bin/node \
  --grant-max-depth 1 --grant-max-breadth 4 --grant-max-total 5 --grant-expires 1h \
  --no-tty --detach -- sleep 3600
```

## 1. What can I do?

```shell
mcp lead tools/list
mcp lead get_limits
```

```text
["create_sandbox","wait","exec","get_file","get_output","stop_sandbox","list_children","get_limits"]
{"isError":false,"can_create_children":true,"can_create_tools":true,"children_created":0,"depth":0,"grant_id":"gr-1ffdaff2","has_grant":true,"max_breadth":4,"max_depth":1,"max_total":5,"total":1}
```

- **Tools are always listed.** Whether a call is allowed is decided when you
  make it; `get_limits` says what is allowed **right now**.

## 2. Create an agent child and a tool sandbox

```shell
mcp lead create_sandbox '{"name":"w1","command":["sleep","600"]}'
mcp lead create_sandbox '{"name":"t1","kind":"tool"}'
```

```text
{"isError":false,"can_create_children":false,"depth":1,"handle":"w1","idle_timeout_s":null,"kind":"agent","lifetime":"until_stopped"}
{"isError":false,"can_create_children":false,"depth":1,"handle":"t1","idle_timeout_s":600,"kind":"tool","lifetime":"until_stopped"}
```

- **Agent** (`w1`): runs its own task; the parent watches it.
- **Tool** (`t1`): runs no agent; the parent runs commands in it.
- Both run the **lead's image**, CPU and memory, with no GPU.

## 3. Use them

Run a command in the tool sandbox, and read a file from the agent child:

```shell
mcp lead exec '{"handle":"t1","command":["uname","-sm"]}'
mcp lead get_file '{"handle":"w1","path":"/etc/hostname"}'
```

```text
{"isError":false,"exit_code":0,"handle":"t1","stderr":"","stdout":"Linux aarch64\n"}
{"isError":false,"content":"8d02bb9e04b5\n","handle":"w1","path":"/etc/hostname"}
```

A one-shot child that exits, and `wait` for it:

```shell
mcp lead create_sandbox '{"name":"once","command":["sh","-c","echo done; exit 0"],"lifetime":"until_exit"}'
mcp lead wait '{"handle":"once","timeout_s":60}'
```

```text
{"isError":false,"exit_code":0,"handle":"once","phase":"SANDBOX_PHASE_COMPLETED","timed_out":false}
```

List and stop:

```shell
mcp lead list_children
mcp lead stop_sandbox '{"handle":"t1"}'
```

```text
{"isError":false,"children":[{"depth":1,"handle":"w1","kind":"agent",...},{"depth":1,"handle":"once",...},{"depth":1,"handle":"t1","kind":"tool",...}]}
{"isError":false,"handle":"t1","stopped":true}
```

- A child that has **exited** leaves only its **exit code**; read files
  while it runs. A results channel is future work.

### Collect an agent child's result

**`wait` reporting READY does not mean the result exists.** READY means the
child's sandbox is up; its agent may still be working. Use an explicit "done"
signal: the child writes its result to a temporary file and **renames** it into
place (a rename is atomic, so the parent never reads half a file), and keeps
running. The parent polls `get_file` until the file is there:

```shell
mcp lead create_sandbox '{"name":"rev","command":["sh","-c","sleep 20; echo \"review: LGTM, 2 nits\" > /tmp/result.tmp && mv /tmp/result.tmp /tmp/result.txt; sleep 600"]}'
mcp lead wait '{"handle":"rev","timeout_s":60}'
mcp lead get_file '{"handle":"rev","path":"/tmp/result.txt"}'
```

```text
{"isError":false,...,"handle":"rev","kind":"agent","lifetime":"until_stopped"}
{"isError":false,"exit_code":null,"handle":"rev","phase":"SANDBOX_PHASE_READY","timed_out":false}
{"isError":true,"error":"FILE_ERROR","message":"cat: /tmp/result.txt: No such file or directory"}
```

Not there yet. Poll until it is, then stop the child:

```shell
until mcp lead get_file '{"handle":"rev","path":"/tmp/result.txt"}' | grep -q '"isError":false'; do sleep 2; done
mcp lead get_file '{"handle":"rev","path":"/tmp/result.txt"}'
mcp lead stop_sandbox '{"handle":"rev"}'
```

```text
{"isError":false,"content":"review: LGTM, 2 nits\n","handle":"rev","path":"/tmp/result.txt"}
{"isError":false,"handle":"rev","stopped":true}
```

- **Don't use `lifetime: until_exit` for results you still need to read:** the
  files go away when the child exits.

## 4. Refusals, and why

| You try | You get | Why |
|---|---|---|
| `curl` instead of Node | `PROGRAM_NOT_ALLOWED` | the grant names `/usr/local/bin/node` |
| `w1` creates an agent | `LIMIT_REACHED` | `w1` is at `max_depth` |
| `exec` on an agent | `WRONG_KIND` | `exec` is for tool sandboxes |
| `get_file` on your own parent | `NOT_YOUR_CHILD` | you act only on your descendants |

```shell
openshell sandbox exec lead -- curl -s -X POST http://openshell.local/mcp -d '{}'
mcp w1 create_sandbox '{"name":"g1","command":["true"]}'
mcp lead exec '{"handle":"w1","command":["id"]}'
mcp lead get_file '{"handle":"lead","path":"/etc/hostname"}'
```

```text
PROGRAM_NOT_ALLOWED: calls from '/usr/bin/curl' are not allowed; this tree's grant allows '/usr/local/bin/node'
{"isError":true,"error":"LIMIT_REACHED","message":"depth: 1 of 1"}
{"isError":true,"error":"WRONG_KIND","message":"'w1' is an agent sandbox; exec is for tool sandboxes"}
{"isError":true,"error":"NOT_YOUR_CHILD","message":"'lead' is not your child"}
```

`w1` still reaches the gateway, sees the same tools, and can create **tool**
sandboxes (tutorial 07):

```shell
mcp w1 get_limits
```

```text
{"isError":false,"can_create_children":false,"can_create_tools":true,"children_created":0,"depth":1,...}
```

Clean up: `openshell sandbox delete lead`.

## What you learned

- Agents reach the gateway at **`http://openshell.local/mcp`** with **no
  credential**; the supervisor adds the identity and program.
- **Agents** run their own task (`wait`, `get_file`); **tools** run your
  commands (`exec`).
- Every refusal names its reason, so an agent can adapt in one turn.
