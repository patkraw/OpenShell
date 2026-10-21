# 01 · CLI: your first tree

**Goal:** start a sandbox that may create children, then manage its **grant**
with the CLI: list, show, widen, revoke, attach one later, and delete a whole
tree.

Setup: [00](00-setup.md), plus the shell setup from the [index](README.md).

## 1. Start a sandbox with a grant

```shell
openshell sandbox create --name lead --from pi-agent:local \
  --policy examples/gateway-mcp-poc/lead.yaml \
  --grant-program /usr/local/bin/node \
  --grant-max-depth 1 --grant-max-breadth 2 --grant-max-total 3 --grant-expires 1h \
  --no-tty --detach -- sleep 3600
```

```text
Created sandbox: lead
```

The **grant** is the only approval:

- **`--grant-max-depth 1`**: the lead's children cannot create agents.
- **`--grant-max-breadth 2`**: at most two children per parent.
- **`--grant-max-total 3`**: at most three sandboxes in the tree, the lead included.
- **`--grant-program`** (required): only Node (Pi runs on Node) may call the gateway MCP.
- **`lead.yaml`** is the most any child of the lead can get.

```shell
openshell grant list
```

```text
GRANT          HOLDER           DEPTH BREADTH     TOTAL PROGRAM                USER
gr-2762cacc    lead                 1       2       1/3 /usr/local/bin/node    unauthenticated-local-dev
```

Your grant id differs from the one shown. Keep it in a variable for the next
steps (the name `GID` is taken in zsh, so use `GRANT_ID`):

```shell
GRANT_ID=$(openshell grant list | awk '$2=="lead"{print $1}')
echo "$GRANT_ID"
```

Show one grant in detail:

```shell
openshell grant get "$GRANT_ID"
```

```text
Grant:       gr-2762cacc
Holder:      lead
Max depth:   1
Max breadth: 2
Total:       1/3
Program:     /usr/local/bin/node
User:        unauthenticated-local-dev
```

An unknown id gives `NOT_FOUND: no grant '<id>'`.

## 2. Let the lead create children

Here we play the lead's agent with the `mcp` helper (tutorial 02 goes deeper):

```shell
mcp lead create_sandbox '{"name":"w1","command":["sleep","600"]}'
mcp lead create_sandbox '{"name":"t1","kind":"tool"}'
openshell sandbox children lead
openshell sandbox list --tree
```

```text
NAME                 KIND   LIFETIME       DEPTH
t1                   tool   until_stopped  1
w1                   agent  until_stopped  1

lead
├─ t1  (tool, depth 1)
└─ w1  (agent, depth 1)
```

- `sandbox children` shows one sandbox's direct children; `sandbox list --tree`
  shows every tree in the workspace.

A third child goes over **`max_breadth`**:

```shell
mcp lead create_sandbox '{"name":"w2","command":["sleep","600"]}'
```

```text
{"isError":true,"error":"LIMIT_REACHED","message":"breadth: 2 of 2"}
```

## 3. Widen the grant

```shell
openshell grant update "$GRANT_ID" --max-breadth 3 --max-total 4
mcp lead create_sandbox '{"name":"w2","command":["sleep","600"]}'
```

```text
✓ Grant gr-2762cacc updated: max_depth=1 max_breadth=3 max_total=4 (applies from the next call)
{"isError":false,"can_create_children":false,"depth":1,"handle":"w2","kind":"agent",...}
```

- **Applies on the next call.** No restart, no policy change.
- `update` changes only depth, breadth and total. For `expires`, `program` or
  `child_policy`, revoke the grant and create a new one (step 4).

## 4. Revoke it, then replace it

```shell
openshell grant revoke "$GRANT_ID"
mcp lead create_sandbox '{"name":"w3","command":["sleep","600"]}'
```

```text
✓ Grant gr-2762cacc revoked: its tree can create no more children
{"isError":true,"error":"NOT_ALLOWED","message":"grant revoked"}
```

The tree keeps running, and the lead can still use the children it has:
`wait`, `get_file`, `exec` on tool children and `stop_sandbox` keep working.
Only new creates are refused. Give it a **new** grant, for example with a
different budget or program:

```shell
openshell grant create lead --program /usr/local/bin/node \
  --max-depth 1 --max-breadth 4 --max-total 5 --expires 1h
GRANT_ID=$(openshell grant list | awk '$2=="lead"{print $1}')
openshell grant get "$GRANT_ID"
mcp lead create_sandbox '{"name":"w3","command":["sleep","600"]}'
```

```text
✓ Grant gr-f76157c2 attached to lead: max_depth=1 max_breadth=4 max_total=5 program=/usr/local/bin/node
Grant:       gr-f76157c2
Holder:      lead
Max depth:   1
Max breadth: 4
Total:       4/5
Program:     /usr/local/bin/node
User:        unauthenticated-local-dev
{"isError":false,"can_create_children":false,"depth":1,"handle":"w3","kind":"agent",...}
```

- **The running children count** toward the new grant (`4/5`: the lead and
  its three children `w1`, `t1` and `w2`).
- Replacing works only after a **revoke** or once the grant has **expired**.
  While a grant is active, `grant create` says `ALREADY_EXISTS: 'lead' already
  holds grant ...; revoke it first to replace it`.
- Only the user who set a grant (or a Workspace Admin) can see, change, revoke
  or replace it.

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

- Only **top-level** sandboxes can get one this way: a child is covered by its
  tree's grant.

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
w3     2026-10-14 22:43:05  Ready
plain  2026-10-14 22:43:06  Ready
p1     2026-10-14 22:43:07  Ready

✓ Sandbox lead deletion accepted; cleanup is pending

NAME   CREATED              PHASE
plain  2026-10-14 22:43:06  Ready
p1     2026-10-14 22:43:07  Ready
```

**Deleting a parent stops its children first** (`w1`, `t1`, `w2`, `w3` are gone).
Clean up: `openshell sandbox delete plain`.

## What you learned

- A **grant** is set at create (`--grant-*` flags) or attached later (`grant create`),
  and only the user can change it.
- **`grant update`** and **`revoke`** take effect on the tree's next call.
- **Deleting a parent** deletes its whole subtree.
