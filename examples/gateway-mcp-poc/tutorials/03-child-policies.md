# 03 · Child policies

**Goal:** give children **less** access than their parent, and see how the
gateway proves it.

Setup: [00](00-setup.md), plus the shell setup from the [index](README.md).

## The rule

- **A child can never get more than its parent has**, at any depth:
  `child ≤ parent ≤ global maximum`.
- The parent's agent writes the child's policy in `create_sandbox`; the
  gateway's **policy prover** checks it against the parent's current policy.
- The agent may narrow **every section the prover checks**:
  - `network_policies`
  - `filesystem_policy`
  - `process`
  - `landlock`
- A section it leaves out: **filesystem, process, Landlock** are copied from
  the parent; **network** is empty (no network unless asked for).

Start a lead (its policy, `lead.yaml`, allows GitHub `GET /repos/**` and
`example.com`):

```shell
openshell sandbox create --name lead --from pi-agent:local \
  --policy examples/gateway-mcp-poc/lead.yaml \
  --grant-program /usr/local/bin/node \
  --grant-max-depth 1 --grant-max-breadth 5 --grant-max-total 6 --grant-expires 1h \
  --no-tty --detach -- sleep 3600
```

## 1. Narrow the network

A reader that may only read GitHub:

```shell
mcp lead create_sandbox '{"name":"reader","command":["sleep","600"],"policy":{"network_policies":{"github_read":{"name":"github_read","endpoints":[{"host":"api.github.com","port":443,"protocol":"rest","enforcement":"enforce","rules":[{"allow":{"method":"GET","path":"/repos/**"}}]}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
```

```text
{"isError":false,"can_create_children":false,"depth":1,"handle":"reader","kind":"agent",...}
```

A child with **no network** at all:

```shell
mcp lead create_sandbox '{"name":"offline","command":["sleep","600"],"policy":{}}'
openshell sandbox get offline --policy-only | grep -A3 network_policies || echo '(no network rules)'
```

```text
{"isError":false,...,"handle":"offline",...}
(no network rules)
```

## 2. Ask for more than the parent has

The lead may only `GET`; a child that wants `POST` is refused:

```shell
mcp lead create_sandbox '{"name":"writer","command":["sleep","600"],"policy":{"network_policies":{"gh":{"name":"gh","endpoints":[{"host":"api.github.com","port":443,"protocol":"rest","enforcement":"enforce","rules":[{"allow":{"method":"POST","path":"/repos/**"}}]}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
```

```text
{"isError":true,"error":"EXCEEDS_CEILING","message":"child policy exceeds the parent's policy;
 counterexample: Network { host: \"api.github.com\", port: 443, protocol: Rest,
 method: Some(\"POST\"), path: Some(\"/repos/a\"), ... }. A child cannot get more than you have.
 Either narrow the child's policy and retry, or, if the task needs this access and policy
 proposals are enabled in this sandbox, first ask for it for yourself: ... then retry create_sandbox."}
```

- The **counterexample** is a request the child could make but the parent
  can't (`POST /repos/a`).
- The **hint** points to the proposal flow ([tutorial 04](04-asking-for-more.md)).

## 3. Narrow the filesystem

Make `/tmp` read-only for a child (the lead has it writable):

```shell
mcp lead create_sandbox '{"name":"fsro","command":["sleep","600"],"policy":{"filesystem_policy":{"include_workdir":true,"read_only":["/bin","/usr","/lib","/proc","/dev/urandom","/etc","/var/log","/tmp"],"read_write":["/dev/null"]}}}'
```

```text
{"isError":false,...,"handle":"fsro",...}
```

Make `/etc` writable (the lead has it read-only):

```shell
mcp lead create_sandbox '{"name":"fsrw","command":["sleep","600"],"policy":{"filesystem_policy":{"include_workdir":true,"read_only":["/bin","/usr","/lib","/proc","/dev/urandom","/var/log"],"read_write":["/tmp","/dev/null","/etc"]}}}'
```

```text
{"isError":true,"error":"UNSUPPORTED_RULES","message":"boundary filesystem authority depends on an unresolved image workdir"}
```

- **Refused either way.** The prover is careful with filesystem paths: it
  proves moving the parent's paths to read-only or dropping them, and refuses
  what it cannot resolve.

## 4. `child_policy=inherit`: every child gets the parent's policy

```shell
openshell sandbox create --name ilead --from pi-agent:local \
  --policy examples/gateway-mcp-poc/lead.yaml \
  --grant-program /usr/local/bin/node \
  --grant-max-depth 1 --grant-max-breadth 2 --grant-max-total 3 --grant-expires 1h --grant-child-policy inherit \
  --no-tty --detach -- sleep 3600
mcp ilead create_sandbox '{"name":"ic1","command":["sleep","600"],"policy":{}}'
mcp ilead create_sandbox '{"name":"ic1","command":["sleep","600"]}'
```

```text
{"isError":true,"error":"INVALID_ARGUMENT","message":"this tree's grant gives every child its parent's policy; omit policy"}
{"isError":false,...,"handle":"ic1",...}
```

## 5. A parent with MCP rules

[`lead-with-mcp.yaml`](lead-with-mcp.yaml) allows one tool of a remote MCP
server (`get_issue`) plus `example.com`. The prover cannot check MCP rules, so:

```shell
openshell sandbox create --name mlead --from pi-agent:local \
  --policy examples/gateway-mcp-poc/tutorials/lead-with-mcp.yaml \
  --grant-program /usr/local/bin/node \
  --grant-max-depth 1 --grant-max-breadth 2 --grant-max-total 3 --grant-expires 1h \
  --no-tty --detach -- sleep 3600
mcp mlead create_sandbox '{"name":"mc1","command":["sleep","600"],"policy":{"network_policies":{"example":{"name":"example","endpoints":[{"host":"example.com","port":443}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
mcp mlead create_sandbox '{"name":"mc2","command":["sleep","600"],"policy":{"network_policies":{"github_mcp":{"name":"github_mcp","endpoints":[{"host":"api.githubcopilot.com","port":443,"path":"/mcp","protocol":"mcp","enforcement":"enforce","rules":[{"allow":{"method":"tools/call","tool":"get_issue"}}]}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
```

```text
{"isError":false,...,"handle":"mc1",...}
{"isError":true,"error":"UNSUPPORTED_RULES","message":"candidate policy rule 'github_mcp' uses authority outside the initial model"}
```

- The parent can still create children for **everything else**.
- It **cannot pass on** its MCP rule in `narrow` mode.
- **Rewriting the child's rule as REST does not help on its own.** A REST rule
  on the server's `/mcp` path is checked like any rule, against the parent's
  policy, and this parent allows that server only through the MCP rule the
  prover cannot see:

```shell
mcp mlead create_sandbox '{"name":"mc3","command":["sleep","600"],"policy":{"network_policies":{"github_mcp":{"name":"github_mcp","endpoints":[{"host":"api.githubcopilot.com","port":443,"protocol":"rest","enforcement":"enforce","rules":[{"allow":{"method":"POST","path":"/mcp"}}]}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
```

```text
{"isError":true,"error":"EXCEEDS_CEILING","message":"child policy exceeds the parent's policy; counterexample: Network { ... host: \"api.githubcopilot.com\", ... method: Some(\"POST\"), path: Some(\"/mcp\") }. ..."}
```

  For a child to get a whole MCP server as REST, the **parent** must also allow
  that REST access (`POST` on `/mcp`, and `GET`/`DELETE` if the child gets them),
  for example by holding the server as a REST rule itself:

```yaml
  github_mcp_rest:
    name: github_mcp_rest
    endpoints:
      - host: api.githubcopilot.com
        port: 443
        protocol: rest
        enforcement: enforce
        rules:
          - allow: {method: POST, path: /mcp}
          - allow: {method: GET, path: /mcp}
    binaries:
      - path: /usr/local/bin/node
```

- **`inherit` does pass MCP rules down.** With `--grant-child-policy inherit`
  the child receives the parent's policy unchanged, MCP rules included; the
  proof is skipped because the policy is the parent's own. Started from
  `lead-with-mcp.yaml` with `inherit`, a child's policy (`openshell sandbox get
  <child> --policy-only`) contains the same `github_mcp` rule with
  `protocol: mcp` and `tool: get_issue`.

## 6. Grandchildren are bounded by their own parent

```shell
openshell sandbox create --name dlead --from pi-agent:local \
  --policy examples/gateway-mcp-poc/lead.yaml \
  --grant-program /usr/local/bin/node \
  --grant-max-depth 2 --grant-max-breadth 2 --grant-max-total 4 --grant-expires 1h \
  --no-tty --detach -- sleep 3600
mcp dlead create_sandbox '{"name":"mid","command":["sleep","600"],"policy":{"network_policies":{"github_read":{"name":"github_read","endpoints":[{"host":"api.github.com","port":443,"protocol":"rest","enforcement":"enforce","rules":[{"allow":{"method":"GET","path":"/repos/**"}}]}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
mcp dlead wait '{"handle":"mid","timeout_s":60}'
mcp mid create_sandbox '{"name":"leaf","command":["true"],"policy":{"network_policies":{"example":{"name":"example","endpoints":[{"host":"example.com","port":443}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
```

```text
{"isError":false,"can_create_children":true,"depth":1,"handle":"mid",...}
{"isError":false,"exit_code":null,"handle":"mid","phase":"SANDBOX_PHASE_READY","timed_out":false}
{"isError":true,"error":"EXCEEDS_CEILING","message":"... counterexample: Network { host: \"example.com\", port: 443, protocol: L4, ... }. ..."}
```

- `mid` must be **ready** before its agent can call the gateway, hence the
  `wait`.
- `dlead` has `example.com`, but **`mid` doesn't**, so `mid` can't give it to
  `leaf`. Each level can only narrow.

Clean up:

```shell
for s in lead ilead mlead dlead; do openshell sandbox delete $s; done
```

## What you learned

- Children are proved against their **parent's current policy**; each level
  can only narrow.
- Agents may narrow **network, filesystem, process and Landlock**; refusals
  carry a **counterexample**.
- **`inherit`** copies the parent's policy, MCP rules included. In `narrow`
  mode MCP rules stay with the parent, and a REST rewrite only works if the
  parent allows that REST access too.
