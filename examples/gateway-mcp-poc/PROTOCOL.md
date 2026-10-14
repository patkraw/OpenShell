# Gateway MCP protocol

The contract between an MCP client (an agent, a harness, an SDK) and the
OpenShell gateway's `/mcp` endpoint, as built in this proof of concept. The
protocol code is `crates/openshell-gateway-mcp/src/lib.rs`; every decision is
made behind the `SandboxApi` trait in `crates/openshell-server/src/children.rs`.

## Transport

| | |
|---|---|
| Protocol | MCP over JSON-RPC 2.0 |
| Transport | MCP streamable HTTP, **sessionless**: one `POST /mcp` carries one JSON-RPC message and gets one `application/json` response |
| `GET /mcp` | `405`: the server opens no server-sent events stream |
| Notifications | a message without `id` gets `202` and no body |
| MCP revision | the server echoes the client's `protocolVersion`; default `2025-11-25` |
| Session id | none; any gateway replica can serve any request |

## Where to connect, and who you are

| Caller | URL | Identity |
|---|---|---|
| Agent or code inside a sandbox | `http://openshell.local/mcp` | The sandbox. The supervisor serves `openshell.local`, checks the calling program against the door rule (`403 door_denied` otherwise) and adds the sandbox's token. The caller sends no credential. |
| Agent or code on the host | the gateway URL plus `/mcp`, e.g. `http://127.0.0.1:18080/mcp` | The user, from the gateway login (OIDC bearer or mTLS; the dev gateway accepts a local user) |

An unauthenticated request gets `401`.

## Methods

| Method | Result |
|---|---|
| `initialize` | `protocolVersion`, `capabilities: {tools: {listChanged: false}}`, `serverInfo`, `instructions` |
| `ping` | `{}` |
| `tools/list` | the tools **this caller** may use (see below) |
| `tools/call` | a tool result (see below) |
| anything else | JSON-RPC error `-32601` (method not found); invalid JSON gives `-32700` |

## Tools per caller

The list is computed on every `tools/list` from the caller's grant:

| Caller | Tools |
|---|---|
| User (host) | `create_sandbox`, `grant_list`, `grant_update` |
| Sandbox with a grant and budget left | `create_sandbox`, `wait`, `exec`, `get_file`, `get_output`, `stop_sandbox`, `list_children`, `get_limits` |
| Sandbox without a grant, at its maximum depth, or out of budget | the same without `create_sandbox` |

A call to a tool that is not in the caller's list is refused with `NOT_ALLOWED`
before it reaches the backend, and the backend refuses it again.

| Tool | Arguments | Returns |
|---|---|---|
| `create_sandbox` | `command`, `kind` (`agent`/`tool`), `lifetime`, `name`, `task`, `policy` (`{"network_policies": {...}}`), `providers`, `request_id`, `idle_timeout_s`; users only: `image`, `grant` | `handle` |
| `wait` | `handle`, `timeout_s` | `phase`, `exit_code` |
| `exec` | `handle`, `command`, `timeout_s` (tool children only) | `stdout`, `stderr`, `exit_code` |
| `get_file` | `handle`, `path` | `content` (max 256 KiB) |
| `get_output` | `handle` | recent log lines |
| `stop_sandbox` | `handle` | `stopped` (its children are stopped first) |
| `list_children` | none | children |
| `get_limits` | none | depth, children and total, used and allowed |
| `grant_list` | none | grants |
| `grant_update` | `grant_id`, `max_depth`, `max_breadth`, `max_total`, `revoke` | the new limits |

Unknown arguments are rejected (`INVALID_ARGUMENT`), so a typo never turns into
a silently ignored field.

## Results and errors

Every `tools/call` returns a JSON-RPC **result**, also when the tool fails:

```json
{
  "content": [{"type": "text", "text": "<the same JSON as structuredContent>"}],
  "structuredContent": {"handle": "w1", "depth": 1},
  "isError": false
}
```

On failure `isError` is `true` and `structuredContent` is
`{"error": "<CODE>", "message": "<why>"}`:

| Code | Meaning |
|---|---|
| `NOT_ALLOWED` | no grant, grant revoked or expired, tool not available to this caller, or a sandbox trying to change grants |
| `LIMIT_REACHED` | depth, breadth or total used up |
| `EXCEEDS_CEILING` | the child policy is wider than the parent's policy (the ceiling); includes the prover's counterexample |
| `UNSUPPORTED_RULES` | the child policy has rules the prover cannot check (e.g. `protocol: mcp`) |
| `GLOBAL_POLICY_ACTIVE` | a gateway-global policy would replace the child's policy |
| `INVALID_ARGUMENT`, `INVALID_POLICY` | bad arguments or policy |
| `NOT_YOUR_CHILD` | the handle is not one of the caller's descendants |
| `WRONG_KIND` | `exec` on an agent child |
| `CREATE_FAILED`, `EXEC_FAILED`, `FILE_ERROR` | the underlying OpenShell operation failed |

## Retries

Send a `request_id` with `create_sandbox`. A repeat with the same id returns the
first result (`"repeated_request": true`) and is counted once against the grant.

## Real exchanges

Captured with `capture-protocol.sh` against a dev gateway (Docker driver),
commit of this file's branch. Responses are unedited.

### initialize

```http
POST /mcp
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"doc","version":"0"}}}
```

```http
HTTP 200 application/json
{
  "id": 1,
  "jsonrpc": "2.0",
  "result": {
    "capabilities": {
      "tools": {
        "listChanged": false
      }
    },
    "instructions": "Create and manage OpenShell sandboxes. Call get_limits to see what you may create.",
    "protocolVersion": "2025-11-25",
    "serverInfo": {
      "name": "openshell-gateway",
      "version": "0.0.0"
    }
  }
}
```

### notifications/initialized (a notification: no id, no body back)

```http
POST /mcp
{"jsonrpc":"2.0","method":"notifications/initialized"}
```

```http
HTTP 202 
(no body)
```

### tools/list as the user

```http
POST /mcp
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
```

```http
HTTP 200 application/json
{
  "id": 2,
  "jsonrpc": "2.0",
  "result": {
    "tools": [
      {
        "description": "Create a sandbox. From a sandbox: creates a child that runs your template (image, CPU, memory); `policy` holds only its network_policies and must fit inside your own policy. From the host: creates a top-level sandbox as the user. Returns a handle.",
        "inputSchema": {
          "additionalProperties": false,
          "properties": {
            "command": {
              "description": "Command to run, e.g. [\"pi\",\"-p\",\"...\"]. Optional for kind=tool.",
              "items": {
                "type": "string"
              },
              "type": "array"
            },
            "grant": {
              "description": "Host callers only: let the new sandbox create children, e.g. max_depth=1,max_breadth=2,max_total=3,expires=4h,program=/usr/local/bin/node",
              "type": "string"
            },
            "idle_timeout_s": {
              "description": "Tool sandboxes: stop after this many idle seconds",
              "type": "integer"
            },
            "image": {
              "description": "Host callers only: image for a top-level sandbox",
              "type": "string"
            },
            "kind": {
              "enum": [
                "agent",
                "tool"
              ],
              "type": "string"
            },
            "lifetime": {
              "enum": [
                "until_stopped",
                "until_exit"
              ],
              "type": "string"
            },
            "name": {
              "type": "string"
            },
            "policy": {
              "description": "{\"network_policies\": {...}} in the OpenShell policy schema (REST/L4 rules only)",
              "type": "object"
            },
            "providers": {
              "items": {
                "type": "string"
              },
              "type": "array"
            },
            "request_id": {
              "description": "The same id on a retry returns the first result",
              "type": "string"
            },
            "task": {
              "description": "Task text, also passed as OPENSHELL_TASK",
              "type": "string"
            }
          },
          "type": "object"
        },
        "name": "create_sandbox"
      },
      {
        "description": "List grants (users only).",
        "inputSchema": {
          "additionalProperties": false,
          "properties": {},
          "type": "object"
        },
        "name": "grant_list"
      },
      {
        "description": "Widen, shrink or revoke a grant (users only). Affects new creates only.",
        "inputSchema": {
          "additionalProperties": false,
          "properties": {
            "grant_id": {
              "type": "string"
            },
            "max_breadth": {
              "type": "integer"
            },
            "max_depth": {
              "type": "integer"
            },
            "max_total": {
              "type": "integer"
            },
            "revoke": {
              "type": "boolean"
            }
          },
          "required": [
            "grant_id"
          ],
          "type": "object"
        },
        "name": "grant_update"
      }
    ]
  }
}
```

### tools/call grant_list

```http
POST /mcp
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"grant_list","arguments":{}}}
```

```http
HTTP 200 application/json
{
  "id": 3,
  "jsonrpc": "2.0",
  "result": {
    "content": [
      {
        "text": "{\"grants\":[{\"grant_id\":\"gr-8af697d7\",\"holder\":\"sdk-lead\",\"max_breadth\":2,\"max_depth\":1,\"max_total\":3,\"program\":\"/usr/local/bin/node\",\"total\":2,\"user\":\"unauthenticated-local-dev\"}]}",
        "type": "text"
      }
    ],
    "isError": false,
    "structuredContent": {
      "grants": [
        {
          "grant_id": "gr-8af697d7",
          "holder": "sdk-lead",
          "max_breadth": 2,
          "max_depth": 1,
          "max_total": 3,
          "program": "/usr/local/bin/node",
          "total": 2,
          "user": "unauthenticated-local-dev"
        }
      ]
    }
  }
}
```

### tools/call with an unknown argument

```http
POST /mcp
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"create_sandbox","arguments":{"nmae":"typo"}}}
```

```http
HTTP 200 application/json
{
  "id": 4,
  "jsonrpc": "2.0",
  "result": {
    "content": [
      {
        "text": "{\"error\":\"INVALID_ARGUMENT\",\"message\":\"unknown field `nmae`, expected one of `command`, `kind`, `lifetime`, `name`, `task`, `policy`, `providers`, `request_id`, `idle_timeout_s`, `image`, `grant`\"}",
        "type": "text"
      }
    ],
    "isError": true,
    "structuredContent": {
      "error": "INVALID_ARGUMENT",
      "message": "unknown field `nmae`, expected one of `command`, `kind`, `lifetime`, `name`, `task`, `policy`, `providers`, `request_id`, `idle_timeout_s`, `image`, `grant`"
    }
  }
}
```

### unknown method

```http
POST /mcp
{"jsonrpc":"2.0","id":5,"method":"resources/list"}
```

```http
HTTP 200 application/json
{
  "error": {
    "code": -32601,
    "message": "method not found"
  },
  "id": 5,
  "jsonrpc": "2.0"
}
```

### GET /mcp (no server-sent events stream)

```http
HTTP 405
```

### Inside sdk-lead, through the door: curl is refused

```http
{"error":"door_denied","detail":"binary '/usr/bin/curl' not allowed in policy 'openshell_gateway_mcp' (ancestors: [], cmdline: []). SYMLINK HINT: the binary path is the kernel-resolved target from /proc/<pid>/exe, not the symlink. If your policy specifies a symlink (e.g., /usr/bin/python3) but the actual binary is /usr/bin/python3.11, either: (1) use the canonical path in your policy (run 'readlink -f /usr/bin/python3' inside the sandbox), or (2) ensure symlink resolution is working (check sandbox logs for 'Cannot access container filesystem')"}
HTTP 403
```

### Inside sdk-lead, through the door: node is allowed (tools/list)

```http
HTTP 200
["create_sandbox","wait","exec","get_file","get_output","stop_sandbox","list_children","get_limits"]
```
