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
| MCP revision | supported: `2025-11-25`, `2025-06-18`. `initialize` echoes a supported `protocolVersion`, otherwise answers `2025-11-25`; an unsupported `MCP-Protocol-Version` header gets `400` |
| Origin | a request with an `Origin` header that is not local (`localhost`, `127.0.0.1`, `[::1]`, `openshell.local`) gets `403` |
| Session id | none; any gateway replica can serve any request |

## Where to connect, and who you are

| Caller | URL | Identity |
|---|---|---|
| Agent or code inside a sandbox | `http://openshell.local/mcp` | The sandbox. The supervisor serves `openshell.local` in every sandbox and adds the sandbox's token and the calling program (`x-openshell-caller-program`); the gateway refuses programs other than the grant's with `403 PROGRAM_NOT_ALLOWED`. The caller sends no credential. |
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

The list depends only on the kind of caller, never on the grant, so a cached
list never goes out of date:

| Caller | Tools |
|---|---|
| User (host) | `create_sandbox`, `grant_list`, `grant_create`, `grant_update` |
| Sandbox | `create_sandbox`, `wait`, `exec`, `get_file`, `get_output`, `stop_sandbox`, `list_children`, `get_limits` |

Whether a call is allowed is decided when it is made: a sandbox without a grant,
at its maximum depth or out of budget gets `NOT_ALLOWED` or `LIMIT_REACHED`.
`get_limits` says what the caller may do right now.

| Tool | Arguments | Returns |
|---|---|---|
| `create_sandbox` | `command`, `kind` (`agent`/`tool`), `lifetime`, `name`, `task`, `policy` (`{"network_policies": {...}}`), `providers`, `request_id`, `idle_timeout_s`; users only: `image`, `grant` | `handle` |
| `wait` | `handle`, `timeout_s`, `until_file` (absolute path in the child; keep waiting past ready until it exists or the child exits), `return_file` (with `until_file`: include the file's content) | `phase`, `exit_code`, `file_ready` (with `until_file`), `content` and `truncated` (with `return_file`, max 256 KiB) |
| `exec` | `handle`, `command`, `timeout_s` (tool children only) | `stdout`, `stderr`, `exit_code` |
| `get_file` | `handle`, `path` | `content` (max 256 KiB), `truncated` |
| `get_output` | `handle` | recent log lines of the child (not a durable result; the child's own standard output is not included) |
| `stop_sandbox` | `handle` | `stopped` (its children are stopped first) |
| `list_children` | none | children |
| `get_limits` | none | depth, children and total, used and allowed |
| `grant_list` | none | the grants the caller set, or administers |
| `grant_create` | `sandbox`, `grant` (same form as `create_sandbox`'s `grant`) | the new grant; replaces a revoked or expired grant for the running tree; refused with `ALREADY_EXISTS` while the sandbox holds an active grant, `NOT_ALLOWED` if it is a child |
| `grant_update` | `grant_id`, `max_depth`, `max_breadth`, `max_total`, `revoke` | the new limits; `NOT_ALLOWED` unless the caller set the grant or is a Workspace Admin |

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
| `INVALID_ARGUMENT`, `INVALID_POLICY` | bad arguments or policy, or a `request_id` reused with different arguments |
| `STALE_ADMISSION` | the grant or the parent's policy changed while the child was being admitted; retry |
| `NOT_YOUR_CHILD` | the handle is not one of the caller's descendants |
| `IN_PROGRESS` | a `create_sandbox` with the same `request_id` from the same parent is still launching; retry shortly |
| `WRONG_KIND` | `exec` on an agent child |
| `CREATE_FAILED`, `EXEC_FAILED`, `FILE_ERROR` | the underlying OpenShell operation failed |

## Retries

Send a `request_id` with `create_sandbox`. A repeat with the same id **from the
same parent** returns the first result (`"repeated_request": true`) and is
counted once against the grant. A repeat that arrives while the first is still
launching gets `IN_PROGRESS`; it never creates a second child. Ids are scoped to
the parent, so siblings may use the same id without colliding.

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
        "description": "Create a sandbox. From a sandbox: creates a child that runs your template (image, CPU, memory); `policy` may narrow any section (network_policies, filesystem_policy, process, landlock) and must fit inside your own policy. From the host: creates a top-level sandbox as the user. Returns a handle.",
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
        "description": "Widen, shrink or revoke a grant (users only). Applies from the next call in its tree.",
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

### Inside sdk-lead, through the door: curl is refused by the gateway

The door is open, but the grant names `/usr/local/bin/node`:

```http
PROGRAM_NOT_ALLOWED: calls from '/usr/bin/curl' are not allowed; this tree's grant allows '/usr/local/bin/node'
HTTP 403
```

### Inside sdk-lead, through the door: node is allowed (tools/list)

```http
HTTP 200
["create_sandbox","wait","exec","get_file","get_output","stop_sandbox","list_children","get_limits"]
```
