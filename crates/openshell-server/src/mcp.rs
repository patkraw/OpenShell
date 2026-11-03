// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! MCP endpoint for agents in root sandboxes (RFC 0021).
//!
//! `POST /mcp` carries one JSON-RPC message and gets one JSON reply; there are
//! no MCP sessions. Each tool is a thin wrapper over an existing RPC, sent
//! through the connection's own gRPC stack, so authentication, authorization,
//! rate limits and interceptors apply exactly as for a direct gRPC call. The
//! request carries [`McpOrigin`], which is the only way a sandbox caller can
//! reach the user-facing methods in [`SANDBOX_TOOL_METHODS`].

use crate::auth::audit::{self, RequestOrigin};
use crate::auth::identity::IdentityProvider;
use crate::auth::principal::Principal;
use crate::multiplex::CallerAuthenticator;
use axum::Extension;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use openshell_core::ObjectName;
use openshell_core::proto::open_shell_client::OpenShellClient;
use openshell_core::proto::{
    CreateSandboxRequest, ExecSandboxRequest, ListSandboxesRequest, Sandbox, SandboxPhase,
    SandboxSpec,
};
use serde_json::{Value, json};
use tower::util::BoxCloneSyncService;
use tracing::warn;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type GrpcStack = BoxCloneSyncService<
    http::Request<tonic::body::Body>,
    http::Response<tonic::body::Body>,
    BoxError,
>;

const MCP_PATH: &str = "/mcp";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18"];
/// Largest request body read, checked only after the caller is authenticated.
const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;

/// Methods a sandbox caller may reach, and only with [`McpOrigin`].
const SANDBOX_TOOL_METHODS: [&str; 5] = [
    "/openshell.v1.OpenShell/CreateSandbox",
    "/openshell.v1.OpenShell/ListSandboxes",
    "/openshell.v1.OpenShell/ExecSandbox",
    "/openshell.v1.OpenShell/DeleteSandbox",
    "/openshell.v1.OpenShell/GetSandboxLogs",
];

/// Marks a gRPC request the `/mcp` endpoint built in process. Only Rust code
/// can set request extensions, so a network caller can never carry it.
#[derive(Clone, Copy, Debug)]
pub struct McpOrigin;

pub fn is_sandbox_tool_method(path: &str) -> bool {
    SANDBOX_TOOL_METHODS.contains(&path)
}

/// One gateway connection's caller resolution and gRPC stack.
#[derive(Clone)]
pub struct McpConnection {
    auth: CallerAuthenticator,
    grpc: GrpcStack,
    create_wait: CreateWait,
}

/// How long `create_sandbox` waits for a child to become ready, and how often
/// it looks.
#[derive(Clone, Copy)]
struct CreateWait {
    limit: std::time::Duration,
    first_poll: std::time::Duration,
    poll: std::time::Duration,
}

impl Default for CreateWait {
    fn default() -> Self {
        Self {
            limit: std::time::Duration::from_secs(45),
            first_poll: std::time::Duration::from_secs(1),
            poll: std::time::Duration::from_secs(2),
        }
    }
}

impl McpConnection {
    pub fn new<S>(auth: CallerAuthenticator, grpc: S) -> Self
    where
        S: tower::Service<
                http::Request<tonic::body::Body>,
                Response = http::Response<tonic::body::Body>,
            > + Clone
            + Send
            + Sync
            + 'static,
        S::Error: Into<BoxError>,
        S::Future: Send + 'static,
    {
        use tower::ServiceExt;
        Self {
            auth,
            grpc: BoxCloneSyncService::new(grpc.map_err(Into::into)),
            create_wait: CreateWait::default(),
        }
    }

    /// Shorten the create wait so tests need not sleep for real.
    #[cfg(test)]
    pub fn with_short_create_wait(mut self) -> Self {
        self.create_wait = CreateWait {
            limit: std::time::Duration::from_millis(50),
            first_poll: std::time::Duration::from_millis(5),
            poll: std::time::Duration::from_millis(10),
        };
        self
    }
}

pub fn router() -> Router {
    Router::new().route(
        MCP_PATH,
        post(handle).get(|| async { StatusCode::METHOD_NOT_ALLOWED }),
    )
}

async fn handle(
    Extension(connection): Extension<McpConnection>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    if let Some(version) = headers.get("mcp-protocol-version")
        && !version
            .to_str()
            .is_ok_and(|v| SUPPORTED_PROTOCOL_VERSIONS.contains(&v))
    {
        return (StatusCode::BAD_REQUEST, "unsupported MCP-Protocol-Version").into_response();
    }
    // Authenticate from the headers before reading the body.
    let Ok(principal) = connection.auth.authenticate(&headers, MCP_PATH).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match &principal {
        Principal::Sandbox(_) => {}
        Principal::Anonymous => return StatusCode::UNAUTHORIZED.into_response(),
        Principal::User(user) if user.identity.provider == IdentityProvider::LocalDev => {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        Principal::User(_) => {
            return (StatusCode::FORBIDDEN, "MCP for users is not available yet").into_response();
        }
        _ => {
            return (StatusCode::FORBIDDEN, "MCP is available to sandboxes only").into_response();
        }
    }
    let Ok(body) = axum::body::to_bytes(body, MAX_REQUEST_BYTES).await else {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body is larger than the 2 MiB /mcp accepts",
        )
            .into_response();
    };

    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return rpc_error(&Value::Null, -32700, "parse error");
    };
    if message.is_array() {
        return rpc_error(&Value::Null, -32600, "batch requests are not supported");
    }
    // Notifications (no id) get no response body.
    let Some(id) = message.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "initialize" => rpc_ok(
            &id,
            json!({
                "protocolVersion": negotiate_protocol_version(
                    params.get("protocolVersion").and_then(Value::as_str),
                ),
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "openshell-gateway", "version": openshell_core::VERSION},
                "instructions": "Create and manage child sandboxes of this sandbox.",
            }),
        ),
        "ping" => rpc_ok(&id, json!({})),
        "tools/list" => rpc_ok(&id, json!({"tools": tool_definitions()})),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !TOOLS.iter().any(|tool| tool.name == name) {
                return rpc_error(&id, -32602, &format!("unknown tool '{name}'"));
            }
            let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
            let result = match name {
                "create_sandbox" => create_child(&connection, &headers, &arguments).await,
                "exec" => exec_in_child(&connection, &headers, &arguments).await,
                "list" => caller_children(&connection, &headers)
                    .await
                    .map(|children| json!({"sandboxes": children.iter().map(child_summary).collect::<Vec<_>>()})),
                _ => Err(ToolError::not_available(name)),
            };
            let origin = RequestOrigin::new(connection.auth.connection_source(), &headers);
            let details = audit_details(name, &arguments);
            match result {
                Ok(content) => {
                    audit::emit_mcp_tool_call(&origin, &principal, name, None, &details);
                    rpc_ok(&id, tool_result(&content, false))
                }
                Err(ToolError::Unauthenticated) => StatusCode::UNAUTHORIZED.into_response(),
                Err(ToolError::Refused(error)) => {
                    audit::emit_mcp_tool_call(
                        &origin,
                        &principal,
                        name,
                        Some(error.code),
                        &details,
                    );
                    rpc_ok(&id, tool_result(&error.to_json(), true))
                }
            }
        }
        _ => rpc_error(&id, -32601, "method not found"),
    }
}

struct Tool {
    name: &'static str,
    description: &'static str,
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "create_sandbox",
        description: "Create a child sandbox of this sandbox.",
    },
    Tool {
        name: "list",
        description: "List this sandbox's child sandboxes.",
    },
    Tool {
        name: "exec",
        description: "Run a command in a child sandbox. If it times out, the command may still be running; delete the child to stop everything.",
    },
    Tool {
        name: "delete",
        description: "Delete a child sandbox and everything running in it.",
    },
    Tool {
        name: "logs",
        description: "Read a child sandbox's recent logs.",
    },
];

fn tool_definitions() -> Vec<Value> {
    TOOLS
        .iter()
        .map(|tool| {
            let schema = match tool.name {
                "create_sandbox" => json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Name of the child sandbox."},
                        "template": {"type": "string", "description": "Optional sandbox template made available to agents."},
                        "providers": {"type": "array", "items": {"type": "string"}, "description": "Optional subset of this sandbox's providers; all of them when omitted."},
                    },
                    "required": ["name"],
                    "additionalProperties": false,
                }),
                "exec" => json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Child sandbox to run in."},
                        "command": {"type": "array", "items": {"type": "string"}, "description": "Program and arguments; not a shell string."},
                        "workdir": {"type": "string"},
                        "stdin": {"type": "string", "description": "Text passed on stdin."},
                        "stdin_base64": {"type": "string", "description": "Bytes passed on stdin, base64-encoded."},
                        "timeout_s": {"type": "integer", "minimum": 1, "maximum": MAX_EXEC_TIMEOUT_S},
                        "background": {"type": "boolean"},
                    },
                    "required": ["name", "command"],
                    "additionalProperties": false,
                }),
                _ => json!({"type": "object"}),
            };
            json!({"name": tool.name, "description": tool.description, "inputSchema": schema})
        })
        .collect()
}

const DEFAULT_EXEC_TIMEOUT_S: u64 = 30;
const MAX_EXEC_TIMEOUT_S: u64 = 45;
/// How long past `timeout_s` the tool waits for the gateway's exit event.
const EXEC_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
const MAX_STDIN_BYTES: usize = 512 * 1024;
/// Bytes kept from each end of each output stream.
const OUTPUT_EDGE_BYTES: usize = 16 * 1024;
/// Exit code the gateway reports when `execution_timeout` fires.
const GATEWAY_TIMEOUT_EXIT_CODE: i32 = 124;
/// Largest argument list recorded in the audit event.
const MAX_AUDIT_ARGV_BYTES: usize = 1024;

struct ExecArgs {
    name: String,
    command: Vec<String>,
    workdir: String,
    stdin: Vec<u8>,
    timeout: std::time::Duration,
}

fn parse_exec_args(arguments: &Value) -> Result<ExecArgs, ToolError> {
    let args = arguments_object(arguments)?;
    for key in args.keys() {
        if key == "env" {
            return Err(ToolError::invalid("'env' is only available to users"));
        }
        if ![
            "name",
            "command",
            "workdir",
            "stdin",
            "stdin_base64",
            "timeout_s",
            "background",
        ]
        .contains(&key.as_str())
        {
            return Err(ToolError::invalid(&format!("unknown argument '{key}'")));
        }
    }
    if args.get("background").and_then(Value::as_bool) == Some(true) {
        return Err(ToolError::Refused(Refusal {
            code: "not_available",
            message: "background jobs are not available yet".to_string(),
            retryable: false,
        }));
    }
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ToolError::invalid("'name' is required"))?;
    let command = args
        .get("command")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
        })
        .filter(|command| !command.is_empty())
        .ok_or_else(|| ToolError::invalid("'command' must be a nonempty list of strings"))?;
    let workdir = args
        .get("workdir")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stdin = match (args.get("stdin"), args.get("stdin_base64")) {
        (Some(_), Some(_)) => {
            return Err(ToolError::invalid(
                "send 'stdin' or 'stdin_base64', not both",
            ));
        }
        (Some(text), None) => text
            .as_str()
            .ok_or_else(|| ToolError::invalid("'stdin' must be a string"))?
            .as_bytes()
            .to_vec(),
        (None, Some(encoded)) => {
            use base64::Engine;
            encoded
                .as_str()
                .and_then(|encoded| {
                    base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .ok()
                })
                .ok_or_else(|| ToolError::invalid("'stdin_base64' must be base64"))?
        }
        (None, None) => Vec::new(),
    };
    if stdin.len() > MAX_STDIN_BYTES {
        return Err(ToolError::invalid("stdin is larger than 512 KiB"));
    }
    let timeout_s = match args.get("timeout_s") {
        None => DEFAULT_EXEC_TIMEOUT_S,
        Some(value) => value
            .as_u64()
            .filter(|timeout| (1..=MAX_EXEC_TIMEOUT_S).contains(timeout))
            .ok_or_else(|| ToolError::invalid("'timeout_s' must be between 1 and 45"))?,
    };
    Ok(ExecArgs {
        name: name.to_string(),
        command,
        workdir: workdir.to_string(),
        stdin,
        timeout: std::time::Duration::from_secs(timeout_s),
    })
}

/// Run a command in one of the caller's children and collect its output, for
/// at most `timeout_s` plus a short grace for the gateway's exit event.
async fn exec_in_child(
    connection: &McpConnection,
    headers: &HeaderMap,
    arguments: &Value,
) -> Result<Value, ToolError> {
    use openshell_core::proto::exec_sandbox_event::Payload;

    let args = parse_exec_args(arguments)?;
    let request = ExecSandboxRequest {
        sandbox: args.name,
        command: args.command,
        workdir: args.workdir,
        stdin: args.stdin,
        execution_timeout: Some(prost_types::Duration {
            seconds: i64::try_from(args.timeout.as_secs()).unwrap_or(i64::MAX),
            nanos: 0,
        }),
        ..Default::default()
    };
    let started = tokio::time::Instant::now();
    let deadline = started + args.timeout + EXEC_EXIT_GRACE;
    let mut client = OpenShellClient::new(connection.grpc.clone());
    let call: futures::future::BoxFuture<'_, _> =
        Box::pin(client.exec_sandbox(mcp_request(request, headers)));
    let mut events = call.await.map_err(ToolError::from_status)?.into_inner();
    let (mut stdout, mut stderr) = (CapturedOutput::default(), CapturedOutput::default());
    let mut exit = None;
    loop {
        let next: futures::future::BoxFuture<'_, _> = Box::pin(events.message());
        let Ok(event) = tokio::time::timeout_at(deadline, next).await else {
            warn!("mcp: exec did not end at its timeout");
            break;
        };
        match event.map_err(ToolError::from_status)? {
            None => break,
            Some(event) => match event.payload {
                Some(Payload::Stdout(out)) => stdout.push(&out.data),
                Some(Payload::Stderr(err)) => stderr.push(&err.data),
                Some(Payload::Exit(code)) => {
                    exit = Some((code.exit_code, started.elapsed()));
                    break;
                }
                None => {}
            },
        }
    }
    let exit_code = command_exit_code(exit, args.timeout);
    let (stdout, stdout_truncated, stdout_binary) = stdout.render();
    let (stderr, stderr_truncated, stderr_binary) = stderr.render();
    let mut reply = json!({
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
        "binary_output": stdout_binary || stderr_binary,
        "timed_out": exit_code.is_none(),
    });
    if exit_code.is_none() {
        reply["message"] = json!(
            "the command may still be running in the child; use background jobs for long work, or delete the child to stop everything"
        );
    }
    Ok(reply)
}

/// The command's own exit code, or `None` when it timed out. The gateway
/// reports its timeout as 124; a command can exit 124 too, but only earlier
/// than one second before the timeout.
fn command_exit_code(
    exit: Option<(i32, std::time::Duration)>,
    timeout: std::time::Duration,
) -> Option<i32> {
    match exit {
        Some((GATEWAY_TIMEOUT_EXIT_CODE, elapsed))
            if elapsed + std::time::Duration::from_secs(1) >= timeout =>
        {
            None
        }
        Some((code, _)) => Some(code),
        None => None,
    }
}

/// One output stream: its first and last bytes, and how many there were.
#[derive(Default)]
struct CapturedOutput {
    head: Vec<u8>,
    tail: Vec<u8>,
    total: usize,
}

impl CapturedOutput {
    fn push(&mut self, data: &[u8]) {
        self.total += data.len();
        let into_head = OUTPUT_EDGE_BYTES
            .saturating_sub(self.head.len())
            .min(data.len());
        self.head.extend_from_slice(&data[..into_head]);
        self.tail.extend_from_slice(&data[into_head..]);
        if self.tail.len() > OUTPUT_EDGE_BYTES {
            self.tail.drain(..self.tail.len() - OUTPUT_EDGE_BYTES);
        }
    }

    /// The kept text, whether bytes were omitted, and whether the kept bytes
    /// held invalid UTF-8. Cuts fall on character boundaries.
    fn render(&self) -> (String, bool, bool) {
        if self.head.len() + self.tail.len() == self.total {
            let mut all = self.head.clone();
            all.extend_from_slice(&self.tail);
            let text = String::from_utf8_lossy(&all);
            let binary = matches!(text, std::borrow::Cow::Owned(_));
            return (text.into_owned(), false, binary);
        }
        let head = &self.head[..utf8_end_before_partial(&self.head)];
        let tail = &self.tail[utf8_start_after_partial(&self.tail)..];
        let omitted = self.total - head.len() - tail.len();
        let (head, tail) = (String::from_utf8_lossy(head), String::from_utf8_lossy(tail));
        let binary = matches!(head, std::borrow::Cow::Owned(_))
            || matches!(tail, std::borrow::Cow::Owned(_));
        (
            format!("{head}\n… [{omitted} bytes omitted] …\n{tail}"),
            true,
            binary,
        )
    }
}

/// Length of `bytes` without a multi-byte character cut off at its end.
fn utf8_end_before_partial(bytes: &[u8]) -> usize {
    let len = bytes.len();
    for back in 1..=len.min(4) {
        let byte = bytes[len - back];
        if byte & 0xC0 == 0x80 {
            continue;
        }
        let width = match byte {
            0xF0..=0xF7 => 4,
            0xE0..=0xEF => 3,
            0xC0..=0xDF => 2,
            _ => 1,
        };
        return if width > back { len - back } else { len };
    }
    len
}

/// Offset past continuation bytes left at the start of `bytes` by a cut.
fn utf8_start_after_partial(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take(3)
        .take_while(|byte| *byte & 0xC0 == 0x80)
        .count()
}

/// What the audit event records about a tool call: for `exec`, the target,
/// the argument list (capped) and the working directory; never stdin.
fn audit_details(tool: &str, arguments: &Value) -> Vec<(&'static str, String)> {
    if tool != "exec" {
        return Vec::new();
    }
    let text = |key| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let argv = arguments
        .get("command")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    vec![
        ("target", text("name")),
        (
            "argv",
            truncate_at_char_boundary(&argv, MAX_AUDIT_ARGV_BYTES).to_string(),
        ),
        ("workdir", text("workdir")),
    ]
}

fn truncate_at_char_boundary(text: &str, max_bytes: usize) -> &str {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn arguments_object(arguments: &Value) -> Result<serde_json::Map<String, Value>, ToolError> {
    match arguments {
        Value::Null => Ok(serde_json::Map::new()),
        Value::Object(args) => Ok(args.clone()),
        _ => Err(ToolError::invalid("arguments must be an object")),
    }
}

/// `create_sandbox` arguments a user may send but a sandbox may not.
const USER_ONLY_CREATE_ARGUMENTS: [&str; 8] = [
    "image", "cpu", "memory", "env", "labels", "command", "policy", "parent",
];

struct CreateArgs {
    name: String,
    template: String,
    providers: Vec<String>,
}

fn parse_create_args(arguments: &Value) -> Result<CreateArgs, ToolError> {
    let args = arguments_object(arguments)?;
    for key in args.keys() {
        if USER_ONLY_CREATE_ARGUMENTS.contains(&key.as_str()) {
            return Err(ToolError::invalid(&format!(
                "'{key}' is only available to users"
            )));
        }
        if !["name", "template", "providers"].contains(&key.as_str()) {
            return Err(ToolError::invalid(&format!("unknown argument '{key}'")));
        }
    }
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ToolError::invalid("'name' is required"))?;
    let template = match args.get("template") {
        None => "",
        Some(Value::String(template)) => template,
        Some(_) => return Err(ToolError::invalid("'template' must be a string")),
    };
    let providers = match args.get("providers") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| ToolError::invalid("'providers' must be a list of names"))?,
        Some(_) => return Err(ToolError::invalid("'providers' must be a list of names")),
    };
    Ok(CreateArgs {
        name: name.to_string(),
        template: template.to_string(),
        providers,
    })
}

/// Create a child, or return the caller's existing child of that name, then
/// wait for it. The name is looked up first so a retry still works when the
/// parent is at its child limit.
async fn create_child(
    connection: &McpConnection,
    headers: &HeaderMap,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let args = parse_create_args(arguments)?;
    if find_child(connection, headers, &args.name).await?.is_some() {
        return wait_until_ready(connection, headers, &args.name, true).await;
    }
    let request = CreateSandboxRequest {
        name: args.name.clone(),
        workload_template: args.template,
        spec: Some(SandboxSpec {
            providers: args.providers,
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut client = OpenShellClient::new(connection.grpc.clone());
    let call: futures::future::BoxFuture<'_, _> =
        Box::pin(client.create_sandbox(mcp_request(request, headers)));
    match call.await {
        Ok(_) => wait_until_ready(connection, headers, &args.name, false).await,
        // A create racing the lookup above.
        Err(status) if status.code() == tonic::Code::AlreadyExists => {
            if find_child(connection, headers, &args.name).await?.is_some() {
                wait_until_ready(connection, headers, &args.name, true).await
            } else {
                Err(ToolError::Refused(Refusal {
                    code: "already_exists",
                    message: format!(
                        "a sandbox named '{}' already exists in this workspace",
                        args.name
                    ),
                    retryable: false,
                }))
            }
        }
        Err(status) => Err(ToolError::from_status(status)),
    }
}

/// Poll the caller's children until `name` is ready or failed, for at most
/// the create wait. Still provisioning at the end is not an error.
async fn wait_until_ready(
    connection: &McpConnection,
    headers: &HeaderMap,
    name: &str,
    already_existed: bool,
) -> Result<Value, ToolError> {
    let wait = connection.create_wait;
    let deadline = tokio::time::Instant::now() + wait.limit;
    let mut pause = wait.first_poll;
    loop {
        let Some(child) = find_child(connection, headers, name).await? else {
            return Err(ToolError::Refused(Refusal {
                code: "not_found",
                message: format!("sandbox '{name}' not found"),
                retryable: false,
            }));
        };
        let mut reply = child_summary(&child);
        reply["already_existed"] = json!(already_existed);
        match SandboxPhase::try_from(child.phase()).unwrap_or(SandboxPhase::Unknown) {
            SandboxPhase::Ready => return Ok(reply),
            SandboxPhase::Error => {
                return Err(ToolError::Refused(Refusal {
                    code: "failed_precondition",
                    message: format!("child sandbox '{name}' failed: {}", failure_message(&child)),
                    retryable: false,
                }));
            }
            _ => {}
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            reply["ready"] = json!(false);
            reply["timed_out"] = json!(true);
            reply["message"] =
                json!("call create_sandbox again with the same name to keep waiting");
            return Ok(reply);
        }
        tokio::time::sleep(pause.min(deadline - now)).await;
        pause = wait.poll;
    }
}

fn failure_message(sandbox: &Sandbox) -> String {
    sandbox
        .status
        .as_ref()
        .and_then(|status| {
            status
                .conditions
                .iter()
                .find(|condition| condition.r#type == "Ready")
        })
        .map(|condition| condition.message.clone())
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| "no reason was recorded".to_string())
}

async fn find_child(
    connection: &McpConnection,
    headers: &HeaderMap,
    name: &str,
) -> Result<Option<Sandbox>, ToolError> {
    Ok(caller_children(connection, headers)
        .await?
        .into_iter()
        .find(|child| child.object_name() == name))
}

/// Every child of the calling sandbox, following page tokens to the end.
async fn caller_children(
    connection: &McpConnection,
    headers: &HeaderMap,
) -> Result<Vec<Sandbox>, ToolError> {
    let mut client = OpenShellClient::new(connection.grpc.clone());
    let mut children = Vec::new();
    let mut page_token = String::new();
    loop {
        let request = ListSandboxesRequest {
            page_size: i32::try_from(crate::pagination::MAX_PAGE_SIZE).unwrap_or(i32::MAX),
            page_token,
            ..Default::default()
        };
        // Boxed so the handler future is provably `Send`; tonic's generic
        // error bound otherwise trips a higher-ranked lifetime check.
        let call: futures::future::BoxFuture<'_, _> =
            Box::pin(client.list_sandboxes(mcp_request(request, headers)));
        let page = call.await.map_err(ToolError::from_status)?.into_inner();
        children.extend(page.sandboxes);
        if page.next_page_token.is_empty() {
            break;
        }
        page_token = page.next_page_token;
    }
    Ok(children)
}

fn child_summary(sandbox: &Sandbox) -> Value {
    let phase = SandboxPhase::try_from(sandbox.phase()).unwrap_or(SandboxPhase::Unknown);
    let created_at = sandbox
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.created_time.as_ref())
        .map(ToString::to_string);
    json!({
        "name": sandbox.object_name(),
        "state": phase.as_str_name().trim_start_matches("SANDBOX_PHASE_").to_ascii_lowercase(),
        "created_at": created_at,
    })
}

/// A gRPC request for the in-process stack: the caller's own credential and
/// the `/mcp` marker.
fn mcp_request<T>(message: T, headers: &HeaderMap) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    for name in [http::header::AUTHORIZATION.as_str(), "x-request-id"] {
        if let Some(value) = headers
            .get(name)
            .and_then(|value| tonic::metadata::AsciiMetadataValue::try_from(value.as_bytes()).ok())
        {
            request.metadata_mut().insert(name, value);
        }
    }
    request.extensions_mut().insert(McpOrigin);
    request
}

enum ToolError {
    /// The caller's credential stopped working mid-call; answered with HTTP 401.
    Unauthenticated,
    Refused(Refusal),
}

struct Refusal {
    code: &'static str,
    message: String,
    retryable: bool,
}

impl Refusal {
    fn to_json(&self) -> Value {
        json!({"error": self.code, "message": self.message, "retryable": self.retryable})
    }
}

impl ToolError {
    fn invalid(message: &str) -> Self {
        Self::Refused(Refusal {
            code: "invalid_argument",
            message: message.to_string(),
            retryable: false,
        })
    }

    fn not_available(name: &str) -> Self {
        Self::Refused(Refusal {
            code: "not_available",
            message: format!("'{name}' is not available yet"),
            retryable: false,
        })
    }

    /// Map a gRPC status to the agent-facing error. Internal errors can carry
    /// store or driver detail, so their text is logged, not returned.
    fn from_status(status: tonic::Status) -> Self {
        use tonic::Code;
        let (code, retryable) = match status.code() {
            Code::Unauthenticated => return Self::Unauthenticated,
            Code::InvalidArgument => ("invalid_argument", false),
            Code::NotFound => ("not_found", false),
            Code::AlreadyExists => ("already_exists", false),
            Code::FailedPrecondition => ("failed_precondition", false),
            Code::PermissionDenied => ("permission_denied", false),
            Code::ResourceExhausted => ("limit_reached", false),
            Code::Unavailable | Code::Aborted => ("unavailable", true),
            Code::DeadlineExceeded => ("timed_out", true),
            _ => {
                let request_id = uuid::Uuid::new_v4();
                warn!(%request_id, error = %status.message(), "mcp: internal error");
                return Self::Refused(Refusal {
                    code: "internal",
                    message: format!(
                        "internal error (request {request_id}); the operator can find it in the gateway logs"
                    ),
                    retryable: true,
                });
            }
        };
        Self::Refused(Refusal {
            code,
            message: status.message().to_string(),
            retryable,
        })
    }
}

fn tool_result(content: &Value, is_error: bool) -> Value {
    let text = content
        .get("message")
        .filter(|_| is_error)
        .and_then(Value::as_str)
        .map_or_else(|| content.to_string(), str::to_string);
    json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": content,
        "isError": is_error,
    })
}

/// Echo a supported protocol version, otherwise offer the newest one.
fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|v| SUPPORTED_PROTOCOL_VERSIONS.iter().find(|s| **s == v))
        .copied()
        .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0])
}

/// Browsers attach `Origin`; MCP servers must refuse foreign origins to stop
/// DNS rebinding. Requests without `Origin` (agents, the sandbox door) and
/// local origins are accepted.
fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(http::header::ORIGIN) else {
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

fn rpc_ok(id: &Value, result: Value) -> Response {
    axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Response {
    axum::Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::authenticator::test_support::MockAuthenticator;
    use crate::auth::authenticator::{Authentication, AuthenticatorChain};
    use crate::auth::identity::Identity;
    use crate::auth::principal::{SandboxIdentitySource, SandboxPrincipal, UserPrincipal};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn sandbox_principal(sandbox_id: &str) -> Principal {
        Principal::Sandbox(SandboxPrincipal {
            sandbox_id: sandbox_id.to_string(),
            source: SandboxIdentitySource::BootstrapJwt {
                issuer: "openshell-gateway:test".to_string(),
                auth_epoch: None,
            },
            trust_domain: Some("openshell".to_string()),
        })
    }

    fn user(provider: IdentityProvider) -> Principal {
        Principal::User(UserPrincipal {
            identity: Identity {
                subject: "user".to_string(),
                display_name: None,
                roles: vec!["openshell-user".to_string()],
                scopes: Vec::new(),
                provider,
            },
        })
    }

    /// A connection whose callers resolve to `caller` (or are refused) and
    /// whose gRPC stack must never be reached.
    fn connection(caller: Option<Principal>) -> McpConnection {
        let outcome = caller.map_or(Ok(Authentication::NotApplicable), |principal| {
            Ok(Authentication::Accepted(principal))
        });
        let chain = AuthenticatorChain::new(vec![Arc::new(MockAuthenticator::returning(outcome))]);
        let unreachable = tower::service_fn(|_: http::Request<tonic::body::Body>| async {
            panic!("the gRPC stack must not be called");
            #[allow(unreachable_code)]
            Ok::<_, std::convert::Infallible>(http::Response::new(tonic::body::Body::empty()))
        });
        let auth = crate::multiplex::AuthGrpcRouter::new(unreachable, Some(chain), None)
            .caller_authenticator();
        McpConnection::new(auth, unreachable)
    }

    async fn post_mcp(
        connection: McpConnection,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> (StatusCode, Option<Value>) {
        let mut request = http::Request::post(MCP_PATH).header("content-type", "application/json");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = router()
            .layer(Extension(connection))
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).ok())
    }

    fn rpc(method: &str, params: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .unwrap()
    }

    #[tokio::test]
    async fn callers_without_a_sandbox_credential_are_turned_away() {
        let initialize = || rpc("initialize", json!({"protocolVersion": "2025-06-18"}));
        for (caller, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(Principal::Anonymous), StatusCode::UNAUTHORIZED),
            (
                Some(user(IdentityProvider::LocalDev)),
                StatusCode::UNAUTHORIZED,
            ),
            (Some(user(IdentityProvider::Oidc)), StatusCode::FORBIDDEN),
            (Some(user(IdentityProvider::Mtls)), StatusCode::FORBIDDEN),
        ] {
            let (status, _) = post_mcp(connection(caller), &[], initialize()).await;
            assert_eq!(status, expected);
        }
    }

    #[tokio::test]
    async fn initialize_negotiates_a_supported_version() {
        for (requested, answered) in [("2025-06-18", "2025-06-18"), ("1999-01-01", "2025-11-25")] {
            let (status, reply) = post_mcp(
                connection(Some(sandbox_principal("lead"))),
                &[],
                rpc("initialize", json!({"protocolVersion": requested})),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(reply.unwrap()["result"]["protocolVersion"], answered);
        }
    }

    #[tokio::test]
    async fn the_endpoint_refuses_what_it_does_not_serve() {
        let lead = || connection(Some(sandbox_principal("lead")));
        // A foreign browser origin, before anything else.
        let (status, _) = post_mcp(
            connection(None),
            &[("origin", "http://attacker.example")],
            rpc("ping", json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, _) = post_mcp(lead(), &[], vec![b' '; MAX_REQUEST_BYTES + 1]).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

        let (_, reply) = post_mcp(lead(), &[], b"[]".to_vec()).await;
        assert_eq!(reply.unwrap()["error"]["code"], -32600);

        let (_, reply) = post_mcp(lead(), &[], rpc("tools/call", json!({"name": "rm"}))).await;
        assert_eq!(reply.unwrap()["error"]["code"], -32602);

        let (status, reply) =
            post_mcp(lead(), &[], rpc("tools/call", json!({"name": "logs"}))).await;
        assert_eq!(status, StatusCode::OK);
        let result = &reply.unwrap()["result"];
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["error"], "not_available");
    }

    #[test]
    fn the_audit_event_is_valid_and_carries_no_token_or_stdin() {
        let headers = HeaderMap::from_iter([(
            http::header::AUTHORIZATION,
            "Bearer canary-secret-token-7d1e".parse().unwrap(),
        )]);
        let details = audit_details(
            "exec",
            &json!({"name": "w1", "command": ["ls", "-l"], "workdir": "/tmp", "stdin": "secret-stdin"}),
        );
        let event = audit::mcp_tool_call_event(
            &RequestOrigin::new(None, &headers),
            &sandbox_principal("lead"),
            "exec",
            Some("not_found"),
            &details,
        );
        let event = serde_json::to_value(event).unwrap();
        assert_eq!(event["api"]["operation"], "exec");
        assert_eq!(event["status_detail"], "not_found");
        assert_eq!(event["unmapped"]["argv"], "ls -l");
        assert_eq!(event["unmapped"]["workdir"], "/tmp");
        let text = event.to_string();
        assert!(!text.contains("canary-secret-token") && !text.contains("secret-stdin"));
        openshell_ocsf_validate::validate_event(&event).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn a_late_124_is_a_timeout_and_an_early_one_is_the_commands() {
        let timeout = std::time::Duration::from_secs(10);
        let at = std::time::Duration::from_millis;
        assert_eq!(command_exit_code(Some((124, at(9_000))), timeout), None);
        assert_eq!(command_exit_code(Some((124, at(10_200))), timeout), None);
        assert_eq!(
            command_exit_code(Some((124, at(8_900))), timeout),
            Some(124)
        );
        assert_eq!(command_exit_code(Some((0, at(10_200))), timeout), Some(0));
        assert_eq!(command_exit_code(None, timeout), None);
    }

    #[tokio::test]
    async fn create_sandbox_refuses_user_only_and_unknown_arguments() {
        for (arguments, message) in [
            (
                json!({"name": "w", "image": "x"}),
                "'image' is only available to users",
            ),
            (
                json!({"name": "w", "parent": "x"}),
                "'parent' is only available to users",
            ),
            (json!({"name": "w", "size": 3}), "unknown argument 'size'"),
            (json!({}), "'name' is required"),
        ] {
            let (_, reply) = post_mcp(
                connection(Some(sandbox_principal("lead"))),
                &[],
                rpc(
                    "tools/call",
                    json!({"name": "create_sandbox", "arguments": arguments}),
                ),
            )
            .await;
            let result = &reply.unwrap()["result"];
            assert_eq!(result["isError"], true);
            assert_eq!(result["structuredContent"]["error"], "invalid_argument");
            assert_eq!(result["structuredContent"]["message"], message);
        }
    }

    #[test]
    fn output_keeps_both_ends_and_cuts_on_character_boundaries() {
        let mut small = CapturedOutput::default();
        small.push(b"hello");
        assert_eq!(small.render(), ("hello".to_string(), false, false));

        // "é" is two bytes; one straddles the head's 16 KiB edge and one the
        // tail's, so neither may turn into a replacement character.
        let mut text = "a".repeat(OUTPUT_EDGE_BYTES - 1);
        text.push('é');
        text.push_str(&"b".repeat(40_000));
        text.push('é');
        text.push_str(&"c".repeat(OUTPUT_EDGE_BYTES - 2));
        let mut output = CapturedOutput::default();
        for chunk in text.as_bytes().chunks(1000) {
            output.push(chunk);
        }
        let (rendered, truncated, binary) = output.render();
        assert!(truncated);
        assert!(!binary, "a cut character is not binary output");
        assert!(!rendered.contains('\u{FFFD}'));
        assert!(rendered.starts_with(&"a".repeat(OUTPUT_EDGE_BYTES - 1)));
        assert!(rendered.ends_with(&"c".repeat(OUTPUT_EDGE_BYTES - 2)));
        assert!(rendered.contains("bytes omitted"));

        let mut binary = CapturedOutput::default();
        binary.push(&[0xff, 0xfe, b'x']);
        assert!(binary.render().2);
    }

    #[test]
    fn the_audit_argument_list_is_capped_on_a_character_boundary() {
        let details = audit_details(
            "exec",
            &json!({"name": "w", "command": ["echo", "é".repeat(1000)], "stdin": "secret"}),
        );
        let argv = &details.iter().find(|(key, _)| *key == "argv").unwrap().1;
        assert!(argv.len() <= MAX_AUDIT_ARGV_BYTES);
        assert!(argv.starts_with("echo é"));
        assert!(details.iter().all(|(_, value)| !value.contains("secret")));
    }

    #[tokio::test]
    async fn exec_refuses_bad_arguments_before_calling_the_gateway() {
        let big = "x".repeat(MAX_STDIN_BYTES + 1);
        let invalid = "invalid_argument";
        for (arguments, error, message) in [
            (
                json!({"name": "w", "command": ["ls"], "background": true}),
                "not_available",
                "background jobs are not available yet",
            ),
            (
                json!({"name": "w", "command": ["ls"], "stdin": big}),
                invalid,
                "stdin is larger than 512 KiB",
            ),
            (
                json!({"name": "w", "command": ["ls"], "stdin": "a", "stdin_base64": "YQ=="}),
                invalid,
                "send 'stdin' or 'stdin_base64', not both",
            ),
            (
                json!({"name": "w", "command": ["ls"], "timeout_s": 46}),
                invalid,
                "'timeout_s' must be between 1 and 45",
            ),
            (
                json!({"name": "w", "command": ["ls"], "env": {}}),
                invalid,
                "'env' is only available to users",
            ),
            (
                json!({"name": "w", "command": ["ls"], "size": 3}),
                invalid,
                "unknown argument 'size'",
            ),
            (
                json!({"name": "w", "command": "ls"}),
                invalid,
                "'command' must be a nonempty list of strings",
            ),
        ] {
            let (_, reply) = post_mcp(
                connection(Some(sandbox_principal("lead"))),
                &[],
                rpc(
                    "tools/call",
                    json!({"name": "exec", "arguments": arguments}),
                ),
            )
            .await;
            let result = &reply.unwrap()["result"];
            assert_eq!(result["isError"], true);
            assert_eq!(result["structuredContent"]["error"], error);
            assert_eq!(result["structuredContent"]["message"], message);
        }
    }

    #[test]
    fn internal_errors_do_not_reach_the_agent() {
        let ToolError::Refused(refusal) =
            ToolError::from_status(tonic::Status::internal("fetch parent sandbox failed: db"))
        else {
            panic!("an internal error is a tool error");
        };
        assert_eq!(refusal.code, "internal");
        // The request id is random hex, so check for the hidden text itself.
        assert!(
            !refusal.message.contains("fetch parent sandbox failed"),
            "{}",
            refusal.message
        );
        assert!(matches!(
            ToolError::from_status(tonic::Status::unauthenticated("expired")),
            ToolError::Unauthenticated
        ));
    }
}
