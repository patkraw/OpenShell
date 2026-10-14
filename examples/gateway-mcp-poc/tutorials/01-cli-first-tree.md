# 01 · CLI: your first tree

**Goal:** start a sandbox that may create children, then manage its **grant**
with the CLI: list, widen, revoke, attach one later, and delete a whole tree.

Setup: [00](00-setup.md), plus the shell setup from the [index](README.md).

## 1. Start a sandbox with a grant

```shell
openshell sandbox create --name lead --from pi-agent:local \
  --policy examples/gateway-mcp-poc/lead.yaml \
  --grant max_depth=1,max_breadth=2,max_total=3,expires=1h,program=/usr/local/bin/node \
  --no-tty --detach -- sleep 3600
```

```text
Created sandbox: lead
```

The **grant** is the only approval:

- **`max_depth=1`**: the lead's children cannot create agents.
- **`max_breadth=2`**: at most two children per parent.
- **`max_total=3`**: at most three sandboxes in the tree, the lead included.
- **`program`**: only Node (Pi runs on Node) may call the gateway MCP.
- **`lead.yaml`** is the most any child of the lead can get.

```shell
openshell grant list
```

```text
GRANT          HOLDER           DEPTH BREADTH     TOTAL PROGRAM                USER
gr-f362e047    lead                 1       2       1/3 /usr/local/bin/node    unauthenticated-local-dev
```

## 2. Let the lead create children

Here we play the lead's agent with the `mcp` helper (tutorial 02 goes deeper):

```shell
mcp lead create_sandbox '{"name":"w1","command":["sleep","600"]}'
mcp lead create_sandbox '{"name":"t1","kind":"tool"}'
openshell sandbox children lead
```

```text
NAME                 KIND   LIFETIME       DEPTH
w1                   agent  until_stopped  1
t1                   tool   until_stopped  1
```

A third child goes over **`max_breadth`**:

```shell
mcp lead create_sandbox '{"name":"w2","command":["sleep","600"]}'
```

```text
{"isError":true,"error":"LIMIT_REACHED","message":"breadth: 2 of 2"}
```

## 3. Widen the grant

```shell
openshell grant update gr-f362e047 --max-breadth 3 --max-total 4
mcp lead create_sandbox '{"name":"w2","command":["sleep","600"]}'
```

```text
✓ Grant gr-f362e047 updated: max_depth=1 max_breadth=3 max_total=4 (applies from the next call)
{"isError":false,"can_create_children":false,"depth":1,"handle":"w2","kind":"agent",...}
```

- **Applies on the next call.** No restart, no policy change.
- `update` changes only depth, breadth and total. For `expires`, `program` or
  `child_policy`, revoke and create a new grant.

## 4. Revoke it

```shell
openshell grant revoke gr-f362e047
mcp lead create_sandbox '{"name":"w3","command":["sleep","600"]}'
```

```text
✓ Grant gr-f362e047 revoked: its tree can create no more children
{"isError":true,"error":"NOT_ALLOWED","message":"grant revoked"}
```

## 5. Attach a grant to a running sandbox

A sandbox started **without** a grant cannot create children:

```shell
openshell sandbox create --name plain --from pi-agent:local \
  --policy examples/gateway-mcp-poc/lead.yaml --no-tty --detach -- sleep 3600
mcp plain create_sandbox '{"name":"p1","command":["sleep","600"]}'
```

```text
{"isError":true,"error":"NOT_ALLOWED","message":"this sandbox has no grant"}
```

Give it one **while it runs**:

```shell
openshell grant create plain --program /usr/local/bin/node \
  --max-depth 1 --max-breadth 2 --max-total 3 --expires 1h
mcp plain create_sandbox '{"name":"p1","command":["sleep","600"]}'
```

```text
✓ Grant gr-c173621f attached to plain: max_depth=1 max_breadth=2 max_total=3 program=/usr/local/bin/node
{"isError":false,"can_create_children":false,"depth":1,"handle":"p1","kind":"agent",...}
```

- Only **top-level** sandboxes **without** a grant can get one this way.

## 6. Delete a tree

```shell
openshell sandbox list
openshell sandbox delete lead
openshell sandbox list
```

```text
NAME   CREATED              PHASE
lead   2026-10-14 22:43:03  Ready
w1     2026-10-14 22:43:04  Ready
t1     2026-10-14 22:43:05  Ready
w2     2026-10-14 22:43:05  Ready
plain  2026-10-14 22:43:06  Ready
p1     2026-10-14 22:43:07  Ready

✓ Sandbox lead deletion accepted; cleanup is pending

NAME   CREATED              PHASE
plain  2026-10-14 22:43:06  Ready
p1     2026-10-14 22:43:07  Ready
```

**Deleting a parent stops its children first** (`w1`, `t1`, `w2` are gone).
Clean up: `openshell sandbox delete plain`.

## What you learned

- A **grant** is set at create (`--grant`) or attached later (`grant create`),
  and only the user can change it.
- **`grant update`** and **`revoke`** take effect on the tree's next call.
- **Deleting a parent** deletes its whole subtree.
