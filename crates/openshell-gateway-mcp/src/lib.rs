// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! MCP surface for creating and managing sandboxes.
//!
//! This crate only speaks MCP: the JSON-RPC handshake, the tool list, argument
//! parsing and result formatting. Every decision (who the caller is, what it
//! may see, whether a create is allowed) belongs to a [`SandboxApi`]
//! implementation. The OpenShell gateway provides one; tests use a fake.
//!
//! Transport: MCP streamable HTTP, sessionless. `POST /mcp` carries one
//! JSON-RPC message and gets one JSON response. `GET /mcp` (server-sent
//! events) is not offered and returns 405.

use async_trait::async_trait;
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::info;

/// MCP revision used when the client does not name one, and offered when it
/// names one this server does not support.
pub const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";

/// MCP revisions this server speaks (Streamable HTTP, JSON responses only).
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18"];

/// Version negotiation (MCP lifecycle): echo a supported version, otherwise
/// answer with the latest one this server supports and let the client decide.
fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|v| SUPPORTED_PROTOCOL_VERSIONS.iter().find(|s| **s == v))
        .copied()
        .unwrap_or(DEFAULT_PROTOCOL_VERSION)
}

/// Browsers attach `Origin`; MCP servers must reject foreign origins to stop
/// DNS-rebinding attacks. Requests without `Origin` (agents, the sandbox door)
/// and local origins are accepted.
fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get("origin") else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let host = origin
        .split_once("://")
        .map_or(origin, |(_, rest)| rest)
        .split(['/', ':'])
        .next()
        .unwrap_or_default();
    matches!(host, "localhost" | "127.0.0.1" | "openshell.local")
        || origin.starts_with("http://[::1]")
}

// ---------------------------------------------------------------------------
// The contract with the backend
// ---------------------------------------------------------------------------

/// An authenticated caller, as the backend sees it.
pub trait CallerInfo: Send + Sync {
    /// Short label for logs, e.g. `sandbox:<id>` or `user:<subject>`.
    fn label(&self) -> String;
}

/// Which tool groups a caller may see. Hiding a tool is a convenience for the
/// model; the backend must still refuse calls it does not allow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Access {
    /// `create_sandbox`.
    pub create_sandbox: bool,
    /// `wait`, `exec`, `get_file`, `get_output`, `stop_sandbox`, `list_children`, `get_limits`.
    pub manage_children: bool,
    /// `grant_list`, `grant_create`, `grant_update` (users only).
    pub grants: bool,
}

/// An error a tool returns to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// `create_sandbox` arguments.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSandboxArgs {
    /// Command to run. Required for agents; tool sandboxes may omit it.
    #[serde(default)]
    pub command: Vec<String>,
    /// `agent` (default) or `tool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `until_stopped` (default) or `until_exit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Task text, passed to the sandbox as `OPENSHELL_TASK`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// For a child: `{"network_policies": {...}}`. For a user: a full policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<String>>,
    /// The same id on a retry returns the first result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Tool sandboxes: stop after this many idle seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_s: Option<u64>,
    /// Users only: the image for a top-level sandbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Users only: a grant for the new sandbox, e.g.
    /// `max_depth=1,max_breadth=2,max_total=3,expires=4h,program=/usr/local/bin/node`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandleArgs {
    pub handle: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitArgs {
    pub handle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecArgs {
    pub handle: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileArgs {
    pub handle: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantUpdateArgs {
    pub grant_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_breadth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoke: Option<bool>,
}

/// Attach a grant to a running top-level sandbox (users only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantCreateArgs {
    /// Name of the sandbox that becomes the holder.
    pub sandbox: String,
    /// Same form as `create_sandbox`'s `grant`, e.g.
    /// `max_depth=1,max_breadth=2,max_total=3,expires=4h,program=/usr/local/bin/node`.
    pub grant: String,
}

/// One tool call, parsed and validated.
#[derive(Debug, Clone, PartialEq)]
pub enum Operation {
    CreateSandbox(CreateSandboxArgs),
    Wait(WaitArgs),
    Exec(ExecArgs),
    GetFile(FileArgs),
    GetOutput(HandleArgs),
    Stop(HandleArgs),
    ListChildren,
    GetLimits,
    GrantList,
    GrantCreate(GrantCreateArgs),
    GrantUpdate(GrantUpdateArgs),
}

/// What the MCP surface needs from OpenShell.
#[async_trait]
pub trait SandboxApi: Send + Sync + 'static {
    type Caller: CallerInfo;

    /// Identify the caller from the request headers.
    async fn authenticate(&self, headers: &HeaderMap) -> Result<Self::Caller, ApiError>;

    /// Which tools to list for this caller.
    async fn access(&self, caller: &Self::Caller) -> Access;

    /// Run one operation. The backend enforces every rule.
    async fn call(&self, caller: &Self::Caller, op: Operation) -> Result<Value, ApiError>;
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Create,
    Manage,
    Grants,
}

struct ToolDef {
    name: &'static str,
    group: Group,
    description: &'static str,
    schema: fn() -> Value,
}

fn handle_schema() -> Value {
    json!({"type": "object", "properties": {"handle": {"type": "string"}}, "required": ["handle"], "additionalProperties": false})
}

const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "create_sandbox",
        group: Group::Create,
        description: "Create a sandbox. From a sandbox: creates a child that runs your template \
            (image, CPU, memory); `policy` may narrow any section (network_policies, filesystem_policy, process, landlock) and must fit inside your \
            own policy. From the host: creates a top-level sandbox as the user. Returns a handle.",
        schema: || {
            json!({"type": "object", "additionalProperties": false, "properties": {
                "command": {"type": "array", "items": {"type": "string"}, "description": "Command to run, e.g. [\"pi\",\"-p\",\"...\"]. Optional for kind=tool."},
                "kind": {"type": "string", "enum": ["agent", "tool"]},
                "lifetime": {"type": "string", "enum": ["until_stopped", "until_exit"]},
                "name": {"type": "string"},
                "task": {"type": "string", "description": "Task text, also passed as OPENSHELL_TASK"},
                "policy": {"type": "object", "description": "{\"network_policies\": {...}} in the OpenShell policy schema (REST/L4 rules only)"},
                "providers": {"type": "array", "items": {"type": "string"}},
                "request_id": {"type": "string", "description": "The same id on a retry returns the first result"},
                "idle_timeout_s": {"type": "integer", "description": "Tool sandboxes: stop after this many idle seconds"},
                "image": {"type": "string", "description": "Host callers only: image for a top-level sandbox"},
                "grant": {"type": "string", "description": "Host callers only: let the new sandbox create children, e.g. max_depth=1,max_breadth=2,max_total=3,expires=4h,program=/usr/local/bin/node"}
            }})
        },
    },
    ToolDef {
        name: "wait",
        group: Group::Manage,
        description: "Wait until a child is ready or has exited.",
        schema: || json!({"type": "object", "additionalProperties": false, "properties": {"handle": {"type": "string"}, "timeout_s": {"type": "integer"}}, "required": ["handle"]}),
    },
    ToolDef {
        name: "exec",
        group: Group::Manage,
        description: "Run a command in one of your tool children (kind \"tool\"). Not allowed on \
            agent children. Every command is logged. Returns stdout, stderr and exit_code.",
        schema: || json!({"type": "object", "additionalProperties": false, "properties": {"handle": {"type": "string"}, "command": {"type": "array", "items": {"type": "string"}}, "timeout_s": {"type": "integer"}}, "required": ["handle", "command"]}),
    },
    ToolDef {
        name: "get_file",
        group: Group::Manage,
        description: "Read a file from a running child, e.g. its result file. Max 256 KiB.",
        schema: || json!({"type": "object", "additionalProperties": false, "properties": {"handle": {"type": "string"}, "path": {"type": "string"}}, "required": ["handle", "path"]}),
    },
    ToolDef {
        name: "get_output",
        group: Group::Manage,
        description: "Recent log lines of a child.",
        schema: handle_schema,
    },
    ToolDef {
        name: "stop_sandbox",
        group: Group::Manage,
        description: "Stop a child. Its own children are stopped first.",
        schema: handle_schema,
    },
    ToolDef {
        name: "list_children",
        group: Group::Manage,
        description: "Your children.",
        schema: || json!({"type": "object", "additionalProperties": false, "properties": {}}),
    },
    ToolDef {
        name: "get_limits",
        group: Group::Manage,
        description: "Your depth, children and total used and allowed.",
        schema: || json!({"type": "object", "additionalProperties": false, "properties": {}}),
    },
    ToolDef {
        name: "grant_list",
        group: Group::Grants,
        description: "List grants (users only).",
        schema: || json!({"type": "object", "additionalProperties": false, "properties": {}}),
    },
    ToolDef {
        name: "grant_create",
        group: Group::Grants,
        description: "Attach a grant to a running top-level sandbox that has none (users only), so \
            it can create children, e.g. grant=max_depth=1,max_breadth=2,max_total=3,expires=4h,program=/usr/local/bin/node",
        schema: || {
            json!({"type": "object", "additionalProperties": false, "properties": {
            "sandbox": {"type": "string"}, "grant": {"type": "string"}},
            "required": ["sandbox", "grant"]})
        },
    },
    ToolDef {
        name: "grant_update",
        group: Group::Grants,
        description: "Widen, shrink or revoke a grant (users only). Applies from the next call in its tree.",
        schema: || {
            json!({"type": "object", "additionalProperties": false, "properties": {
            "grant_id": {"type": "string"}, "max_depth": {"type": "integer"},
            "max_breadth": {"type": "integer"}, "max_total": {"type": "integer"},
            "revoke": {"type": "boolean"}}, "required": ["grant_id"]})
        },
    },
];

fn visible(group: Group, access: Access) -> bool {
    match group {
        Group::Create => access.create_sandbox,
        Group::Manage => access.manage_children,
        Group::Grants => access.grants,
    }
}

/// The tool list for a caller with `access`.
pub fn tools_for(access: Access) -> Vec<Value> {
    TOOLS
        .iter()
        .filter(|t| visible(t.group, access))
        .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": (t.schema)()}))
        .collect()
}

fn args<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|e| ApiError::new("INVALID_ARGUMENT", e.to_string()))
}

/// Parse a `tools/call` into an [`Operation`].
pub fn parse_operation(name: &str, arguments: Value) -> Result<Operation, ApiError> {
    let arguments = if arguments.is_null() {
        json!({})
    } else {
        arguments
    };
    Ok(match name {
        "create_sandbox" => Operation::CreateSandbox(args(arguments)?),
        "wait" => Operation::Wait(args(arguments)?),
        "exec" => {
            let a: ExecArgs = args(arguments)?;
            if a.command.is_empty() {
                return Err(ApiError::new(
                    "INVALID_ARGUMENT",
                    "command must not be empty",
                ));
            }
            Operation::Exec(a)
        }
        "get_file" => Operation::GetFile(args(arguments)?),
        "get_output" => Operation::GetOutput(args(arguments)?),
        "stop_sandbox" => Operation::Stop(args(arguments)?),
        "list_children" => Operation::ListChildren,
        "get_limits" => Operation::GetLimits,
        "grant_list" => Operation::GrantList,
        "grant_create" => Operation::GrantCreate(args(arguments)?),
        "grant_update" => Operation::GrantUpdate(args(arguments)?),
        other => {
            return Err(ApiError::new(
                "NOT_FOUND",
                format!("unknown tool '{other}'"),
            ));
        }
    })
}

fn group_of(op: &Operation) -> Group {
    match op {
        Operation::CreateSandbox(_) => Group::Create,
        Operation::GrantList | Operation::GrantCreate(_) | Operation::GrantUpdate(_) => {
            Group::Grants
        }
        _ => Group::Manage,
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC over HTTP
// ---------------------------------------------------------------------------

/// The `/mcp` route, backed by `api`.
pub fn router<A: SandboxApi>(api: Arc<A>) -> Router {
    Router::new()
        .route(
            "/mcp",
            post(handle::<A>).get(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .with_state(api)
}

async fn handle<A: SandboxApi>(
    State(api): State<Arc<A>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    // After initialize, clients send the negotiated version in this header;
    // an unsupported one is a client error (MCP Streamable HTTP transport).
    if let Some(v) = headers.get("mcp-protocol-version")
        && !v
            .to_str()
            .is_ok_and(|v| SUPPORTED_PROTOCOL_VERSIONS.contains(&v))
    {
        return (StatusCode::BAD_REQUEST, "unsupported MCP-Protocol-Version").into_response();
    }
    let caller = match api.authenticate(&headers).await {
        Ok(c) => c,
        Err(e) if e.code == "UNAUTHENTICATED" => {
            return (StatusCode::UNAUTHORIZED, e.message).into_response();
        }
        Err(e) => {
            return (StatusCode::FORBIDDEN, format!("{}: {}", e.code, e.message)).into_response();
        }
    };
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return rpc_error(Value::Null, -32700, "parse error");
    };
    // Notifications (no id) get no response body.
    let Some(id) = req.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => {
            let version =
                negotiate_protocol_version(params.get("protocolVersion").and_then(Value::as_str));
            rpc_ok(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "openshell-gateway", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "Create and manage OpenShell sandboxes. Call get_limits to see what you may create."
                }),
            )
        }
        "ping" => rpc_ok(id, json!({})),
        "tools/list" => {
            let access = api.access(&caller).await;
            rpc_ok(id, json!({"tools": tools_for(access)}))
        }
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
            let result = match parse_operation(name, arguments) {
                Ok(op) => {
                    // A hidden tool is refused here too; the backend refuses it again.
                    if visible(group_of(&op), api.access(&caller).await) {
                        api.call(&caller, op).await
                    } else {
                        Err(ApiError::new(
                            "NOT_ALLOWED",
                            format!("'{name}' is not available to you"),
                        ))
                    }
                }
                Err(e) => Err(e),
            };
            let is_error = result.is_err();
            let content = match result {
                Ok(v) => v,
                Err(e) => json!({"error": e.code, "message": e.message}),
            };
            info!(tool = %name, caller = %caller.label(), is_error, "gateway mcp: tool call");
            rpc_ok(
                id,
                json!({
                    "content": [{"type": "text", "text": content.to_string()}],
                    "structuredContent": content,
                    "isError": is_error
                }),
            )
        }
        _ => rpc_error(id, -32601, "method not found"),
    }
}

fn rpc_ok(id: Value, result: Value) -> Response {
    json_response(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn rpc_error(id: Value, code: i64, message: &str) -> Response {
    json_response(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
}

fn json_response(v: &Value) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::Mutex;
    use tower::ServiceExt;

    /// A fake backend: callers are named by the `x-caller` header.
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<(String, Operation)>>,
    }

    struct FakeCaller(String);
    impl CallerInfo for FakeCaller {
        fn label(&self) -> String {
            self.0.clone()
        }
    }

    #[async_trait]
    impl SandboxApi for Fake {
        type Caller = FakeCaller;
        async fn authenticate(&self, headers: &HeaderMap) -> Result<FakeCaller, ApiError> {
            headers
                .get("x-caller")
                .and_then(|v| v.to_str().ok())
                .map(|v| FakeCaller(v.to_string()))
                .ok_or_else(|| ApiError::new("UNAUTHENTICATED", "no caller"))
        }
        async fn access(&self, caller: &FakeCaller) -> Access {
            match caller.0.as_str() {
                "user" => Access {
                    create_sandbox: true,
                    manage_children: false,
                    grants: true,
                },
                "lead" => Access {
                    create_sandbox: true,
                    manage_children: true,
                    grants: false,
                },
                _ => Access {
                    create_sandbox: false,
                    manage_children: true,
                    grants: false,
                },
            }
        }
        async fn call(&self, caller: &FakeCaller, op: Operation) -> Result<Value, ApiError> {
            self.calls
                .lock()
                .unwrap()
                .push((caller.0.clone(), op.clone()));
            match op {
                Operation::CreateSandbox(a) => {
                    Ok(json!({"handle": a.name.unwrap_or_else(|| "c1".into())}))
                }
                Operation::GetLimits => Ok(json!({"depth": 0})),
                _ => Err(ApiError::new("NOT_YOUR_CHILD", "nope")),
            }
        }
    }

    async fn rpc(app: &Router, caller: Option<&str>, body: Value) -> (StatusCode, Value) {
        let mut req = Request::post("/mcp").header("content-type", "application/json");
        if let Some(c) = caller {
            req = req.header("x-caller", c);
        }
        let resp = app
            .clone()
            .oneshot(req.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn call(name: &str, args: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}})
    }

    fn names(v: &Value) -> Vec<String> {
        v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn initialize_echoes_protocol_version() {
        let app = router(Arc::new(Fake::default()));
        let (s, v) = rpc(&app, Some("lead"), json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}})).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(v["result"]["serverInfo"]["name"], "openshell-gateway");
    }

    #[tokio::test]
    async fn initialize_offers_latest_for_unknown_version() {
        let app = router(Arc::new(Fake::default()));
        let (s, v) = rpc(&app, Some("lead"), json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2099-01-01"}})).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["result"]["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
    }

    async fn send(app: &Router, header: (&str, &str)) -> StatusCode {
        let req = Request::post("/mcp")
            .header("content-type", "application/json")
            .header("x-caller", "lead")
            .header(header.0, header.1)
            .body(Body::from(
                json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string(),
            ))
            .unwrap();
        app.clone().oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn unsupported_protocol_header_is_400() {
        let app = router(Arc::new(Fake::default()));
        assert_eq!(
            send(&app, ("mcp-protocol-version", "2099-01-01")).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            send(&app, ("mcp-protocol-version", "2025-06-18")).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn foreign_origin_is_403() {
        let app = router(Arc::new(Fake::default()));
        assert_eq!(
            send(&app, ("origin", "https://evil.example")).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send(&app, ("origin", "http://localhost:3000")).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn unauthenticated_is_401() {
        let app = router(Arc::new(Fake::default()));
        let (s, _) = rpc(
            &app,
            None,
            json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
        )
        .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn notifications_get_202() {
        let app = router(Arc::new(Fake::default()));
        let (s, _) = rpc(
            &app,
            Some("lead"),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;
        assert_eq!(s, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn get_is_405() {
        let app = router(Arc::new(Fake::default()));
        let resp = app
            .oneshot(Request::get("/mcp").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn tool_list_depends_on_the_caller() {
        let app = router(Arc::new(Fake::default()));
        let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        let lead = names(&rpc(&app, Some("lead"), list.clone()).await.1);
        let worker = names(&rpc(&app, Some("worker"), list.clone()).await.1);
        let user = names(&rpc(&app, Some("user"), list).await.1);
        assert!(lead.contains(&"create_sandbox".to_string()));
        assert!(!lead.contains(&"grant_update".to_string()));
        assert!(!lead.contains(&"grant_create".to_string()));
        assert!(!worker.contains(&"create_sandbox".to_string()));
        assert!(worker.contains(&"wait".to_string()));
        assert!(user.contains(&"grant_update".to_string()));
        assert!(user.contains(&"grant_create".to_string()));
        assert!(!user.contains(&"exec".to_string()));
    }

    #[tokio::test]
    async fn hidden_tool_is_refused_without_reaching_the_backend() {
        let fake = Arc::new(Fake::default());
        let app = router(fake.clone());
        let (_, v) = rpc(
            &app,
            Some("worker"),
            call("create_sandbox", json!({"command": ["true"]})),
        )
        .await;
        assert_eq!(v["result"]["isError"], true);
        assert_eq!(v["result"]["structuredContent"]["error"], "NOT_ALLOWED");
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn arguments_are_parsed_and_passed_typed() {
        let fake = Arc::new(Fake::default());
        let app = router(fake.clone());
        let (_, v) = rpc(
            &app,
            Some("lead"),
            call(
                "create_sandbox",
                json!({"name": "w1", "kind": "tool", "request_id": "r1"}),
            ),
        )
        .await;
        assert_eq!(v["result"]["structuredContent"]["handle"], "w1");
        let calls = fake.calls.lock().unwrap();
        let Operation::CreateSandbox(a) = &calls[0].1 else {
            panic!("wrong op")
        };
        assert_eq!(a.kind.as_deref(), Some("tool"));
        assert_eq!(a.request_id.as_deref(), Some("r1"));
    }

    #[tokio::test]
    async fn grant_create_is_for_users_only() {
        let fake = Arc::new(Fake::default());
        let app = router(fake.clone());
        let args = json!({"sandbox": "box", "grant": "max_depth=1,program=/usr/local/bin/node"});
        let (_, v) = rpc(&app, Some("lead"), call("grant_create", args.clone())).await;
        assert_eq!(v["result"]["structuredContent"]["error"], "NOT_ALLOWED");
        assert!(fake.calls.lock().unwrap().is_empty());
        rpc(&app, Some("user"), call("grant_create", args)).await;
        let calls = fake.calls.lock().unwrap();
        assert_eq!(
            calls[0].1,
            Operation::GrantCreate(GrantCreateArgs {
                sandbox: "box".into(),
                grant: "max_depth=1,program=/usr/local/bin/node".into(),
            })
        );
    }

    #[tokio::test]
    async fn unknown_arguments_and_tools_are_rejected() {
        let app = router(Arc::new(Fake::default()));
        let (_, v) = rpc(
            &app,
            Some("lead"),
            call("create_sandbox", json!({"bogus": 1})),
        )
        .await;
        assert_eq!(
            v["result"]["structuredContent"]["error"],
            "INVALID_ARGUMENT"
        );
        let (_, v) = rpc(&app, Some("lead"), call("delete_everything", json!({}))).await;
        assert_eq!(v["result"]["structuredContent"]["error"], "NOT_FOUND");
        let (_, v) = rpc(
            &app,
            Some("lead"),
            call("exec", json!({"handle": "t1", "command": []})),
        )
        .await;
        assert_eq!(
            v["result"]["structuredContent"]["error"],
            "INVALID_ARGUMENT"
        );
    }

    #[tokio::test]
    async fn backend_errors_become_tool_errors() {
        let app = router(Arc::new(Fake::default()));
        let (_, v) = rpc(
            &app,
            Some("lead"),
            call("stop_sandbox", json!({"handle": "x"})),
        )
        .await;
        assert_eq!(v["result"]["isError"], true);
        assert_eq!(v["result"]["structuredContent"]["error"], "NOT_YOUR_CHILD");
    }

    #[tokio::test]
    async fn bad_json_and_unknown_methods() {
        let app = router(Arc::new(Fake::default()));
        let req = Request::post("/mcp")
            .header("x-caller", "lead")
            .body(Body::from("{not json"))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let v: Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(v["error"]["code"], -32700);
        let (_, v) = rpc(
            &app,
            Some("lead"),
            json!({"jsonrpc": "2.0", "id": 9, "method": "resources/list"}),
        )
        .await;
        assert_eq!(v["error"]["code"], -32601);
    }
}
