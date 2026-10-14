# 04 · Asking for more

**Goal:** a child needs access its parent doesn't have. Walk the official path:
the **parent asks for it for itself**, you approve, the parent retries. Then see
that later changes to a child's policy follow the same rule.

Setup: [00](00-setup.md), plus the shell setup from the [index](README.md).

## The flow

1. The lead's `create_sandbox` is refused with `EXCEEDS_CEILING` and a **hint**.
2. The lead **proposes the rule for itself** through `http://policy.local`.
3. **You approve** it (`openshell rule approve`).
4. The lead waits until its policy has **reloaded**.
5. The lead **retries**, and the child now fits.

An approval in the harness's own interface does not count: only OpenShell's
approval changes the parent's policy.

## 0. Start a lead and turn on agent proposals

Agent proposals are **off by default**:

```shell
openshell sandbox create --name lead --from pi-agent:local \
  --policy examples/gateway-mcp-poc/lead.yaml \
  --grant max_depth=1,max_breadth=2,max_total=3,expires=1h,program=/usr/local/bin/node \
  --no-tty --detach -- sleep 3600
openshell settings set lead --key agent_policy_proposals_enabled --value true
```

```text
✓ Set sandbox setting agent_policy_proposals_enabled=true for lead (revision 1)
```

Wait about **15 seconds**; the sandbox picks up settings on its next poll.

## 1. Refused

```shell
WRITER='{"name":"writer","command":["sleep","600"],"policy":{"network_policies":{"github_write":{"name":"github_write","endpoints":[{"host":"api.github.com","port":443,"protocol":"rest","enforcement":"enforce","rules":[{"allow":{"method":"POST","path":"/repos/**"}}]}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
mcp lead create_sandbox "$WRITER"
```

```text
{"isError":true,"error":"EXCEEDS_CEILING","message":"child policy exceeds the parent's policy; counterexample: ... method: Some(\"POST\") ...
 ... first ask for it for yourself: read /etc/openshell/skills/policy_advisor.md, submit the narrowest rule to
 http://policy.local/v1/proposals, wait for approval and `policy_reloaded: true`, then retry create_sandbox."}
```

## 2. The lead proposes the rule for itself

```shell
PROPOSAL='{"intent_summary":"Let a child sandbox open GitHub pull requests.","operations":[{"addRule":{"ruleName":"github_write","rule":{"name":"github_write","endpoints":[{"host":"api.github.com","port":443,"protocol":"rest","enforcement":"enforce","rules":[{"allow":{"method":"POST","path":"/repos/**"}}]}],"binaries":[{"path":"/usr/local/bin/node"}]}}}]}'
openshell sandbox exec lead -- curl -s -X POST http://policy.local/v1/proposals \
  -H 'content-type: application/json' -d "$PROPOSAL"
```

```text
{"status":"submitted","accepted_chunks":1,"rejected_chunks":0,"rejection_reasons":[],"accepted_chunk_ids":["ad53c838-4f33-40f2-9f88-68c3a2bf2dc1"]}
```

## 3. You review and approve

```shell
openshell rule get lead
openshell rule approve lead --chunk-id ad53c838-4f33-40f2-9f88-68c3a2bf2dc1
```

```text
  Chunk: ad53c838-4f33-40f2-9f88-68c3a2bf2dc1
  Status: pending
  Rule: github_write
  Rationale: Let a child sandbox open GitHub pull requests.
  Endpoints: api.github.com:443 [L7 rest, allow POST /repos/**]

OK Chunk approved. Policy version: 2, hash: 4b58be6fb98e
```

## 4. The lead waits for the reload

```shell
openshell sandbox exec lead -- curl -s \
  'http://policy.local/v1/proposals/ad53c838-4f33-40f2-9f88-68c3a2bf2dc1/wait?timeout=60'
```

```text
{"chunk_id":"ad53c838-...","status":"approved","rule_name":"github_write",...,"policy_reloaded":true}
```

## 5. Retry

```shell
mcp lead create_sandbox "$WRITER"
```

```text
{"isError":false,"can_create_children":false,"depth":1,"handle":"writer","kind":"agent",...}
```

## 6. Later changes to a child follow the same rule

A child's policy can change after it starts, through its own proposals or
`openshell policy set`. Every such write is **proved against its parent's
current policy**.

The child `writer` asks for `api.openai.com`, which the lead doesn't have:

```shell
openshell settings set writer --key agent_policy_proposals_enabled --value true
# wait ~15 seconds
openshell sandbox exec writer -- curl -s -X POST http://policy.local/v1/proposals \
  -H 'content-type: application/json' \
  -d '{"intent_summary":"reach openai","operations":[{"addRule":{"ruleName":"openai","rule":{"name":"openai","endpoints":[{"host":"api.openai.com","port":443}],"binaries":[{"path":"/usr/local/bin/node"}]}}}]}'
openshell rule approve writer --chunk-id <chunk-id-from-the-response>
```

```text
Error:   × ... message: "EXCEEDS_CEILING: a child sandbox's policy must stay
  │ within its parent's policy (lead); child policy exceeds the parent's
  │ policy; counterexample: Network { ... host: \"api.openai.com\", port: 443 ... }"
```

The same for **you**, setting a wider policy by hand:

```shell
openshell policy set writer --policy examples/gateway-mcp-poc/tutorials/wider-than-lead.yaml
```

```text
Error:   × ... message: "EXCEEDS_CEILING: a child sandbox's policy must stay
  │ within its parent's policy (lead); ... host: \"api.openai.com\" ..."
```

- To give `writer` that access, **widen `lead` first**, then `writer`.
- Children already running are **not re-checked** when their parent's policy
  later **shrinks** (upstream issue #3808).

Clean up: `openshell sandbox delete lead`.

## What you learned

- A parent gets more access **for itself** first, through `policy.local` and
  your approval; then it can pass it on.
- Agent proposals are **off by default** (`agent_policy_proposals_enabled`).
- **Every later write** to a child's policy is held to its parent's policy.
