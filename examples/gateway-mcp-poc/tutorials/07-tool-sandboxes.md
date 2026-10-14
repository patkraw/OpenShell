# 07 · Tool sandboxes

**Goal:** run commands for your agent in separate, disposable sandboxes, even
from the bottom of a tree.

Setup: [00](00-setup.md), plus the shell setup from the [index](README.md).

## What a tool sandbox is

- **No agent** runs in it. Its parent runs commands with **`exec`** and reads
  files with `get_file`.
- Good for **builds, tests and untrusted code**, away from the agent.
- **Stops itself** after 10 idle minutes (or `idle_timeout_s`).
- **Ignores `max_depth`**: an agent with no depth left can still create tool
  sandboxes. They count toward breadth and total.
- **Never** creates children of its own. Every `exec` command is logged.

Start a lead:

```shell
openshell sandbox create --name lead --from pi-agent:local \
  --policy examples/gateway-mcp-poc/lead.yaml \
  --grant max_depth=1,max_breadth=3,max_total=6,expires=1h,program=/usr/local/bin/node \
  --no-tty --detach -- sleep 3600
```

## 1. Create one and run commands

```shell
mcp lead create_sandbox '{"name":"build","kind":"tool"}'
mcp lead exec '{"handle":"build","command":["sh","-c","echo compiling; ls /usr/local/bin | head -3; exit 2"]}'
```

```text
{"isError":false,"can_create_children":false,"depth":1,"handle":"build","idle_timeout_s":600,"kind":"tool",...}
{"isError":false,"exit_code":2,"handle":"build","stderr":"","stdout":"compiling\ncorepack\ndocker-entrypoint.sh\nfd\n"}
```

- You get **stdout, stderr and the exit code** back right away.

## 2. Idle stop

```shell
mcp lead create_sandbox '{"name":"quick","kind":"tool","idle_timeout_s":10}'
sleep 40
openshell sandbox list
```

```text
NAME   CREATED              PHASE
lead   ...                  Ready
build  2026-10-14 22:48:18  Ready
```

- `quick` was stopped after 10 seconds without `exec` or `get_file`.
- A stopped child **still counts toward `max_total`**.

## 3. Tools at the bottom of the tree

`w1` is at `max_depth`: it can't create agents, but it can create tools:

```shell
mcp lead create_sandbox '{"name":"w1","command":["sleep","600"]}'
mcp w1 get_limits
mcp w1 create_sandbox '{"name":"w1-agent","command":["true"]}'
mcp w1 create_sandbox '{"name":"w1-tool","kind":"tool"}'
mcp w1 exec '{"handle":"w1-tool","command":["echo","leaf ran this"]}'
```

```text
{"isError":false,"can_create_children":false,"can_create_tools":true,"depth":1,...}
{"isError":true,"error":"LIMIT_REACHED","message":"depth: 1 of 1"}
{"isError":false,"can_create_children":false,"depth":2,"handle":"w1-tool","idle_timeout_s":600,"kind":"tool",...}
{"isError":false,"exit_code":0,"handle":"w1-tool","stderr":"","stdout":"leaf ran this\n"}
```

A tool sandbox can't create anything:

```shell
mcp w1-tool create_sandbox '{"name":"x","kind":"tool"}'
```

```text
{"isError":true,"error":"NOT_ALLOWED","message":"tool sandboxes cannot create children"}
```

`exec` on an **agent** child is refused with `WRONG_KIND` (tutorial 02).

Clean up: `openshell sandbox delete lead`.

## What you learned

- **Tool sandboxes** run your commands and return output at once; they stop
  when idle.
- They **ignore depth**, so every agent in a tree can have them, within
  breadth and total.
- They **never** create children.
