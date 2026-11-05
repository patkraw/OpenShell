# 04 · Templates

**Goal:** let agents start children on another image, through a template
you publish, see how an agent finds it, and see the check that keeps other
templates out of reach.

Setup: [02](02-mcp-inside-a-root.md) (`lead` running), the `child-pi:local`
image from [00](00-setup.md), plus the shell setup from the
[index](README.md).

An agent cannot name an image. It can name a **template**: a saved image,
resource request and environment that a user made, and marked as usable by
agents with the label `openshell.ai/agent-usable=true`.

## 1. Publish a template for agents

```shell
openshell sandbox template create pi-tutorial --image child-pi:local \
  --label openshell.ai/agent-usable=true --env PI_DEMO=1
```

```text
✓ Created sandbox template pi-tutorial
```

```shell
openshell sandbox template get pi-tutorial
```

```text
Sandbox template:

  Name: pi-tutorial
  Workspace: default
  Id: de0e0101-bd66-4bb9-b0f6-cbd6fe874265
  Resource version: 1
  Created: 2026-11-05 03:15:13
  Labels: openshell.ai/agent-usable=true
  Image: child-pi:local
  Environment: 1
```

`--cpu`, `--memory` and `--gpu` set resources the same way.

## 2. The agent sees it in its tool list

Nobody has to tell the agent the template's name. The root's tool list now
offers it on `create_sandbox`:

```shell
openshell sandbox exec --name lead --no-tty -- curl -s -X POST http://openshell.local/mcp \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' \
  | jq '.result.tools[] | select(.name=="create_sandbox") | {description, template: .inputSchema.properties.template}'
```

```json
{
  "description": "Create a sandbox. Called from a sandbox, it creates a child of that sandbox; called by a user, a root, or a child of `parent`. Templates you may create a child from: pi-tutorial (image child-pi:local). Without a template the child copies your image.",
  "template": {
    "description": "(sandboxes only) Sandbox template made available to agents.",
    "enum": [
      "pi-tutorial"
    ],
    "type": "string"
  }
}
```

The list holds every labelled template in the workspace without driver
configuration, up to 50, with its image but never its environment. Other
labelled templates on your gateway show up here too. Before you publish one,
the `template` argument has no `enum` and the description no list. A user's
tool list, through `openshell mcp serve`, never has one
([06](06-mcp-from-host.md)).

## 3. Start a child from it

```shell
mcp lead create_sandbox '{"name":"pi1","template":"pi-tutorial"}'
mcp lead exec '{"name":"pi1","command":["sh","-c","command -v pi; echo PI_DEMO=$PI_DEMO"]}' | jq -r .stdout
```

```text
/usr/local/bin/pi
PI_DEMO=1
```

The child runs the template's image and environment, with the **parent's**
policy and a subset of the parent's providers.

## 4. Only labelled templates

A template without the label is left out of the tool list, and a create
that names it is refused with the same message as one that does not exist:

```shell
openshell sandbox template create plain --image nvcr.io/nvidia/base/ubuntu:24.04
mcp lead create_sandbox '{"name":"pi2","template":"plain"}'
```

```json
{"error":"failed_precondition","message":"template 'plain' is not available to agents","retryable":false}
```

## 5. Before you publish

Any agent in the workspace can see and start a labelled template. Before you
label one, check two things:

- You are happy for any agent there to run its image.
- Its image matches the roots that will use it. A child keeps its parent's
  policy, which was written for the parent's image: programs at other paths
  are denied network access, and paths the parent's image lacks are not
  available. The images in this folder share one layout for this reason.

## 6. Run a real harness

[`demo-mixed-harnesses.sh`](../demo-mixed-harnesses.sh) starts Claude Code,
Codex and Pi children under one lead from the templates `claude-agent`,
`codex-agent` and `pi-agent`, with the lead policy in
[`images/lead-policy.yaml`](../images/lead-policy.yaml) that names every
harness its children run.

## Clean up

```shell
mcp lead delete '{"name":"pi1"}'
openshell sandbox template delete plain
openshell sandbox template delete pi-tutorial
```

## What you learned

- Agents pick images only through templates labelled
  `openshell.ai/agent-usable=true`.
- The root's tool list names those templates, so the agent finds them
  itself.
- A template sets image, resources and environment; policy and providers
  still come from the parent.
- Publish templates whose image matches the parents' policy.
