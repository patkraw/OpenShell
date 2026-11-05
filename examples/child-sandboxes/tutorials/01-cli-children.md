# 01 · CLI children

**Goal:** create a root sandbox and a child of it with the CLI, list the
tree, see what the CLI refuses, and delete the tree in the right order.

Setup: [00](00-setup.md), plus the shell setup from the [index](README.md).

## 1. Start a root

A root is an ordinary sandbox. Any root can have children; there is nothing
to grant first.

```shell
openshell sandbox create --name lead --from child-base:local --detach
```

```text
Created sandbox: lead
```

Wait until `openshell sandbox list` shows it `Ready`.

## 2. Create a child

`--parent` makes the new sandbox a child of `lead`:

```shell
openshell sandbox create --name w1 --parent lead --detach
```

The child copies its parent's policy. It does **not** copy the parent's
image: a child you create with the CLI runs the gateway's default image
unless you pass `--from` or `--template`. (A child an agent creates over MCP
does get the parent's image; see [02](02-mcp-inside-a-root.md).)

## 3. List the tree

```shell
openshell sandbox list
```

```text
NAME      PARENT    CREATED              PHASE
lead      -         2026-11-05 02:43:28  Ready
w1        lead      2026-11-05 02:43:35  Ready
```

Only one parent's children:

```shell
openshell sandbox list --parent lead
```

```text
NAME    PARENT    CREATED              PHASE
w1      lead      2026-11-05 02:43:35  Ready
```

`openshell sandbox get w1 -o json` shows the parent as
`"parent": {"id": "…", "name": "lead"}`.

## 4. What the CLI refuses

A child always uses its parent's policy:

```shell
openshell sandbox create --name w2 --parent lead --policy examples/child-sandboxes/images/lead-policy.yaml
```

```text
Error:   × child sandboxes use their parent's policy; omit --policy
```

Children cannot have children:

```shell
openshell sandbox create --name gc --parent w1
```

```text
Error:   × code: 'The system is not in a state required for the operation's
  │ execution', message: "only one level of child sandboxes is supported"
```

A child can use only providers its parent has (more in
[05](05-policy-and-providers.md)). `my-github` from [00](00-setup.md) is not
attached to `lead`:

```shell
openshell sandbox create --name w3 --parent lead --provider my-github
```

```text
  │ execution', message: "provider 'my-github' is not attached to parent 'lead'"
```

A parent cannot be deleted while it has children:

```shell
openshell sandbox delete lead
```

```text
! Failed to delete sandbox lead: code: 'The system is not in a state required for the operation's execution', message: "sandbox 'lead' has 1 child sandbox; delete them first"
Error:   × failed to delete 1 sandbox: lead
```

## Clean up

Keep `lead` and `w1` if you go on to 02; they are reused there. Otherwise
delete the children first, then the root:

```shell
openshell sandbox delete w1
openshell sandbox delete lead
```

## What you learned

- `--parent` creates a child; there is one level only.
- A child copies its parent's policy, but a CLI child does not copy its image.
- `sandbox list` shows the tree, and `--parent` filters it.
- Delete children before their parent.
