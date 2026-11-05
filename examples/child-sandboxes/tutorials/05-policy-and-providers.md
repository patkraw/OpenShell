# 05 · Policy and providers

**Goal:** see a child follow its parent: change the parent's policy and
watch every child pick it up, then share a provider with a child and take it
away again.

Setup: [01](01-cli-children.md) and [02](02-mcp-inside-a-root.md) (`lead`,
`w1` and `a1` running), plus the shell setup from the [index](README.md).

## 1. A child has its parent's policy

```shell
for s in lead a1 w1; do openshell sandbox get $s -o json | jq -c '{name, current_policy_version}'; done
```

```json
{"name":"lead","current_policy_version":1}
{"name":"a1","current_policy_version":1}
{"name":"w1","current_policy_version":1}
```

The gateway copies the parent's current policy when it creates a child, and
copies it again whenever the parent's policy changes. Providers add their own
network rules to each sandbox separately; those are never copied.

A child's policy cannot be changed on its own:

```shell
openshell policy set --policy examples/child-sandboxes/tutorials/lead-web.yaml a1
```

```text
Error:   × code: 'The system is not in a state required for the operation's
  │ execution', message: "child sandboxes use their parent's policy; change
  │ the parent's policy instead"
```

## 2. Change the parent's policy

Neither child can reach `example.com` yet:

```shell
openshell sandbox exec --name a1 --no-tty -- curl -s -o /dev/null -w '%{http_code}\n' --max-time 5 https://example.com
```

```text
000
```

[`lead-web.yaml`](lead-web.yaml) is the root's current policy plus a rule
that lets `curl` reach `example.com`. Check that its filesystem paths match
`openshell policy get lead --base` first: a live update can add paths but not
remove them.

```shell
openshell policy set --policy examples/child-sandboxes/tutorials/lead-web.yaml lead --wait
```

```text
✓ Policy version 2 submitted (hash: 5394238c624d)
✓ Policy version 2 loaded (active version: 2)
```

Each child picks up the new version on its next poll, within about 10
seconds:

```shell
for s in lead a1 w1; do openshell sandbox get $s -o json | jq -c '{name, current_policy_version}'; done
openshell sandbox exec --name a1 --no-tty -- curl -s -o /dev/null -w '%{http_code}\n' --max-time 5 https://example.com
```

```json
{"name":"lead","current_policy_version":2}
{"name":"a1","current_policy_version":2}
{"name":"w1","current_policy_version":2}
```

```text
200
```

`openshell policy list a1` shows version 2 `Loaded` and version 1
`Superseded`.

If a parent's new policy fails to load, its children never get that
version. Until the parent loads a good one, the gateway refuses to create its
children or send them new configuration, with
`parent sandbox 'lead' rejected its latest policy`.

A policy that removes filesystem paths, like
[`images/lead-policy.yaml`](../images/lead-policy.yaml), must be given when
the root is created (`sandbox create --policy`); a live update refuses it
with `filesystem read_only path '/bin' cannot be removed on a live sandbox`.

## 3. Share a provider

A child can use only providers its parent has. Attach `my-github` from
[00](00-setup.md) to the parent:

```shell
openshell sandbox provider attach lead my-github --wait
mcp lead create_sandbox '{"name":"gh1","providers":["my-github"]}'
openshell sandbox provider list gh1
```

```text
NAME       TYPE    CREDENTIAL_KEYS   CONFIG_KEYS
my-github  github  1                 0
```

A child gets only the providers its create call names; `a1` has none.

## 4. Take it away again

Detaching a provider from the parent removes it from every child too:

```shell
openshell sandbox provider detach lead my-github --wait
openshell sandbox provider list gh1
```

```text
No providers attached to sandbox gh1.
```

Attaching it to the parent again does not give it back to the child; attach
it to the child as well with `openshell sandbox provider attach gh1
my-github`. Attaching a provider to a child is refused while its parent does
not have it; the CLI shows only `provider attachment failed (The system is
not in a state required for the operation's execution)`. To remove a provider
from one child only, detach it from that child.

## Clean up

```shell
openshell sandbox delete gh1 a1 w1
openshell sandbox delete lead
```

## What you learned

- A child always has its parent's policy and follows every change to it.
- Change policy on the parent; a child's own `policy set` is refused.
- A child's providers are a subset of its parent's, and shrink when the
  parent loses one.
