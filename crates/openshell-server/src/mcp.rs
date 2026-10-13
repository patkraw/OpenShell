// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Gateway MCP endpoint (proof of concept for the "Gateway MCP for creating
//! sandboxes" RFC draft).
//!
//! Agents in a sandbox reach `/mcp` through their supervisor's door at
//! `http://openshell.local/mcp`, which forwards the call with the sandbox's
//! session token. The gateway decides everything:
//!
//! - a sandbox may create children only if its tree has a **grant**, set by the
//!   user when starting the tree's first sandbox (`sandbox create --grant ...`);
//! - every child policy is proved to fit inside the grant's **ceiling**;
//! - depth, breadth and total limits are counted per tree;
//! - `create_sandbox` is hidden from callers that cannot use it;
//! - a repeated `request_id` returns the first result;
//! - stopping a sandbox stops its children first (cascade stop).
//!
//! POC limits: grants and lineage live in memory (lost on gateway restart);
//! no idle/max TTL; no file tools.

use crate::ServerState;
use crate::auth::authenticator::Authentication;
use crate::auth::identity::Identity;
use crate::auth::principal::{Principal, UserPrincipal};
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use openshell_core::proto::{
    CreateSandboxRequest, GetSandboxLogsRequest, Sandbox, SandboxPhase, SandboxPolicy,
    SandboxSpec, workspace_selector,
};
use openshell_core::{ObjectId, ObjectName, ObjectWorkspace};
use openshell_prover::containment::{self, CheckOptions, CheckResult};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// Annotation carrying the grant on a user's `CreateSandbox` request.
pub const GRANT_ANNOTATION: &str = "openshell.io/mcp-grant";
/// Network policy key of the door rule the gateway writes from a grant.
pub const DOOR_RULE_KEY: &str = "openshell_gateway_mcp";
/// Host and port of the supervisor door.
pub const DOOR_HOST: &str = "openshell.local";
pub const DOOR_PORT: u32 = 80;

const PROVER_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_WAIT: Duration = Duration::from_secs(600);

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// A user-set budget for one tree of sandboxes.
#[derive(Debug, Clone)]
pub struct Grant {
    pub id: String,
    pub user: Identity,
    pub holder_id: String,
    /// Levels below the holder. 1 = the holder's children cannot create.
    pub max_depth: u32,
    /// Children per parent.
    pub max_breadth: u32,
    /// Sandboxes in the whole tree, including the holder.
    pub max_total: u32,
    /// Program allowed to use the door, e.g. `/usr/local/bin/node`.
    pub program: String,
    /// The holder's policy at start, before the door rule. Children must fit inside.
    pub ceiling: SandboxPolicy,
    pub expires_at_ms: i64,
    pub total: u32,
    /// request_id -> child sandbox name (first result).
    pub request_ids: HashMap<String, String>,
}

/// Who created whom. Separate from the sandbox record (no owner field).
#[derive(Debug, Clone)]
pub struct Lineage {
    pub sandbox_id: String,
    pub name: String,
    pub workspace: String,
    pub parent_id: Option<String>,
    pub depth: u32,
    pub grant_id: String,
    pub kind: String,
    pub lifetime: String,
    pub children_created: u32,
}

#[derive(Debug, Default)]
struct Inner {
    grants: HashMap<String, Grant>,
    lineage: HashMap<String, Lineage>,
}

#[derive(Debug, Default)]
pub struct McpState {
    inner: Mutex<Inner>,
}

impl McpState {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lineage(&self, sandbox_id: &str) -> Option<Lineage> {
        self.lock().lineage.get(sandbox_id).cloned()
    }

    fn grant(&self, grant_id: &str) -> Option<Grant> {
        self.lock().grants.get(grant_id).cloned()
    }

    /// Children of `sandbox_id`, deepest first, for cascade stop.
    pub fn descendants_deepest_first(&self, sandbox_id: &str) -> Vec<Lineage> {
        let inner = self.lock();
        let mut out = Vec::new();
        let mut frontier = vec![sandbox_id.to_string()];
        while let Some(id) = frontier.pop() {
            for l in inner.lineage.values() {
                if l.parent_id.as_deref() == Some(id.as_str()) {
                    out.push(l.clone());
                    frontier.push(l.sandbox_id.clone());
                }
            }
        }
        out.sort_by(|a, b| b.depth.cmp(&a.depth));
        out
    }

    pub fn forget(&self, sandbox_id: &str) {
        let mut inner = self.lock();
        inner.lineage.remove(sandbox_id);
        inner.grants.retain(|_, g| g.holder_id != sandbox_id);
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Grant spec ("max_depth=1,max_breadth=2,max_total=3,expires=4h,program=/usr/local/bin/node")
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantSpec {
    pub max_depth: u32,
    pub max_breadth: u32,
    pub max_total: u32,
    pub expires: Duration,
    pub program: String,
}

pub fn parse_grant_spec(spec: &str) -> Result<GrantSpec, String> {
    let mut out = GrantSpec {
        max_depth: 1,
        max_breadth: 2,
        max_total: 3,
        expires: Duration::from_secs(4 * 3600),
        program: String::new(),
    };
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| format!("grant: expected key=value, got '{part}'"))?;
        let num = || v.parse::<u32>().map_err(|_| format!("grant: '{k}' must be a number"));
        match k {
            "max_depth" => out.max_depth = num()?,
            "max_breadth" => out.max_breadth = num()?,
            "max_total" => out.max_total = num()?,
            "expires" => out.expires = parse_duration(v)?,
            "program" => out.program = v.to_string(),
            other => return Err(format!("grant: unknown key '{other}'")),
        }
    }
    if out.program.is_empty() || !out.program.starts_with('/') {
        return Err("grant: 'program' must be an absolute path (the harness binary)".into());
    }
    if out.max_total == 0 {
        return Err("grant: max_total must be at least 1".into());
    }
    Ok(out)
}

fn parse_duration(v: &str) -> Result<Duration, String> {
    let (n, unit) = v.split_at(v.trim_end_matches(char::is_alphabetic).len());
    let n: u64 = n.parse().map_err(|_| format!("grant: bad duration '{v}'"))?;
    let secs = match unit {
        "s" | "" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => return Err(format!("grant: bad duration unit in '{v}'")),
    };
    Ok(Duration::from_secs(secs))
}

/// The network rule that lets `program` reach the supervisor door.
pub fn door_rule(program: &str) -> Result<openshell_core::proto::NetworkPolicyRule, String> {
    let yaml = format!(
        "version: 1\nnetwork_policies:\n  {DOOR_RULE_KEY}:\n    name: {DOOR_RULE_KEY}\n    endpoints:\n      - host: {DOOR_HOST}\n        port: {DOOR_PORT}\n    binaries:\n      - path: {program}\n"
    );
    let mut p = openshell_policy::parse_sandbox_policy(&yaml).map_err(|e| e.to_string())?;
    p.network_policies
        .remove(DOOR_RULE_KEY)
        .ok_or_else(|| "door rule missing".to_string())
}

// ---------------------------------------------------------------------------
// Hooks called from CreateSandbox / DeleteSandbox
// ---------------------------------------------------------------------------

/// Before a user's create: if it carries a grant, remember the ceiling and add
/// the door rule. Returns the parsed grant spec and ceiling to register after
/// the sandbox exists.
pub fn prepare_granted_create(
    principal: &Principal,
    annotations: &HashMap<String, String>,
    spec: &mut SandboxSpec,
) -> Result<Option<(GrantSpec, SandboxPolicy)>, tonic::Status> {
    let Some(raw) = annotations.get(GRANT_ANNOTATION) else {
        return Ok(None);
    };
    if !matches!(principal, Principal::User(_)) {
        return Err(tonic::Status::permission_denied("only a user can set a grant"));
    }
    let grant = parse_grant_spec(raw).map_err(tonic::Status::invalid_argument)?;
    let policy = spec.policy.get_or_insert_with(SandboxPolicy::default);
    let ceiling = policy.clone();
    if grant.max_depth > 0 {
        let rule = door_rule(&grant.program).map_err(tonic::Status::internal)?;
        policy.network_policies.insert(DOOR_RULE_KEY.to_string(), rule);
    }
    Ok(Some((grant, ceiling)))
}

pub fn register_granted_sandbox(
    state: &ServerState,
    principal: &Principal,
    sandbox: &Sandbox,
    grant: GrantSpec,
    ceiling: SandboxPolicy,
) {
    let Principal::User(user) = principal else {
        return;
    };
    let grant_id = format!("gr-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let expires_ms = i64::try_from(grant.expires.as_millis()).unwrap_or(i64::MAX);
    let mut inner = state.mcp.lock();
    inner.grants.insert(
        grant_id.clone(),
        Grant {
            id: grant_id.clone(),
            user: user.identity.clone(),
            holder_id: sandbox.object_id().to_string(),
            max_depth: grant.max_depth,
            max_breadth: grant.max_breadth,
            max_total: grant.max_total,
            program: grant.program,
            ceiling,
            expires_at_ms: now_ms().saturating_add(expires_ms),
            total: 1,
            request_ids: HashMap::new(),
        },
    );
    inner.lineage.insert(
        sandbox.object_id().to_string(),
        Lineage {
            sandbox_id: sandbox.object_id().to_string(),
            name: sandbox.object_name().to_string(),
            workspace: sandbox.object_workspace().to_string(),
            parent_id: None,
            depth: 0,
            grant_id: grant_id.clone(),
            kind: "agent".into(),
            lifetime: "until_stopped".into(),
            children_created: 0,
        },
    );
    info!(grant_id = %grant_id, holder = %sandbox.object_name(), "mcp: grant registered");
}

/// Stop a sandbox's children, deepest first, before the sandbox itself.
pub async fn cascade_stop_children(state: &Arc<ServerState>, sandbox_id: &str) {
    for child in state.mcp.descendants_deepest_first(sandbox_id) {
        info!(child = %child.name, parent = ?child.parent_id, "mcp: cascade stop");
        if let Err(e) = state.compute.delete_sandbox_by_id(&child.sandbox_id, &child.name).await {
            warn!(child = %child.name, error = %e, "mcp: cascade delete failed");
        }
        state.mcp.forget(&child.sandbox_id);
    }
}

// ---------------------------------------------------------------------------
// HTTP / JSON-RPC
// ---------------------------------------------------------------------------

pub fn router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/mcp", post(handle_mcp).get(|| async { StatusCode::METHOD_NOT_ALLOWED }))
        .with_state(state)
}

async fn authenticate(state: &ServerState, headers: &HeaderMap) -> Result<Principal, String> {
    if let Some(chain) = crate::multiplex::build_authenticator_chain(state) {
        match chain.authenticate(headers, "/mcp").await {
            Ok(Authentication::Accepted(p)) => return Ok(p),
            Ok(Authentication::NotApplicable | Authentication::Declined(_)) => {}
            Err(rejection) => return Err(rejection.status.message().to_string()),
        }
    }
    if state.config.auth.allow_unauthenticated_users {
        return Ok(crate::multiplex::unauthenticated_dev_user_principal());
    }
    Err("unauthenticated".into())
}

async fn handle_mcp(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(p) => p,
        Err(e) => return (StatusCode::UNAUTHORIZED, e).into_response(),
    };
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return rpc_error(Value::Null, -32700, "parse error");
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = req.get("params").cloned().unwrap_or(Value::Null);

    if req.get("id").is_none() {
        // Notification (e.g. notifications/initialized).
        return StatusCode::ACCEPTED.into_response();
    }
    match method {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2025-11-25")
                .to_string();
            rpc_ok(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "openshell-gateway", "version": "poc"},
                    "instructions": "Create and manage child sandboxes. Call get_limits first."
                }),
            )
        }
        "ping" => rpc_ok(id, json!({})),
        "tools/list" => rpc_ok(id, json!({"tools": tools_for(&state, &principal)})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let result = call_tool(&state, &principal, name, &args).await;
            let (text, is_error) = match result {
                Ok(v) => (v, false),
                Err(e) => (json!({"error": e.code, "message": e.message}), true),
            };
            info!(tool = %name, caller = %caller_label(&principal), is_error, "mcp: tool call");
            rpc_ok(
                id,
                json!({
                    "content": [{"type": "text", "text": text.to_string()}],
                    "structuredContent": text,
                    "isError": is_error
                }),
            )
        }
        _ => rpc_error(id, -32601, "method not found"),
    }
}

fn rpc_ok(id: Value, result: Value) -> Response {
    json_response(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn rpc_error(id: Value, code: i64, message: &str) -> Response {
    json_response(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
}

fn json_response(v: Value) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response()
}

fn caller_label(p: &Principal) -> String {
    match p {
        Principal::User(u) => format!("user:{}", u.identity.subject),
        Principal::Operator(o) => format!("operator:{}", o.identity.subject),
        Principal::Sandbox(s) => format!("sandbox:{}", s.sandbox_id),
        Principal::Peer(_) => "peer".into(),
        Principal::Supervisor(_) => "supervisor".into(),
        Principal::Anonymous => "anonymous".into(),
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ToolError {
    pub code: &'static str,
    pub message: String,
}

fn terr(code: &'static str, message: impl Into<String>) -> ToolError {
    ToolError { code, message: message.into() }
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({"name": name, "description": description, "inputSchema": schema})
}

/// The tool list depends on who is asking (decision: the gateway hides
/// `create_sandbox` from callers that cannot use it).
fn tools_for(state: &ServerState, principal: &Principal) -> Vec<Value> {
    let handle = json!({"type": "object", "properties": {"handle": {"type": "string"}}, "required": ["handle"]});
    let mut tools = Vec::new();
    match principal {
        Principal::Sandbox(s) => {
            if can_create(state, &s.sandbox_id).is_ok() {
                tools.push(tool(
                    "create_sandbox",
                    "Create a child sandbox running an agent or a tool. It runs your image. \
                     `policy` holds only network_policies for the child (REST/L4 rules); it must fit \
                     inside your tree's ceiling. Returns a handle.",
                    json!({"type": "object", "properties": {
                        "command": {"type": "array", "items": {"type": "string"}, "description": "Command to run, e.g. [\"pi\",\"-p\",\"...task...\"]"},
                        "task": {"type": "string", "description": "Task text, also passed as OPENSHELL_TASK"},
                        "policy": {"type": "object", "description": "{\"network_policies\": {...}} in OpenShell policy schema"},
                        "providers": {"type": "array", "items": {"type": "string"}},
                        "kind": {"type": "string", "enum": ["agent", "tool"]},
                        "lifetime": {"type": "string", "enum": ["until_stopped", "until_exit"]},
                        "name": {"type": "string"},
                        "request_id": {"type": "string", "description": "Same id on retry returns the first result"}
                    }, "required": ["command"]}),
                ));
            }
            tools.push(tool("wait", "Wait until a child is ready or has exited.", json!({"type": "object", "properties": {"handle": {"type": "string"}, "timeout_s": {"type": "integer"}}, "required": ["handle"]})));
            tools.push(tool("get_output", "Recent log output of a child.", handle.clone()));
            tools.push(tool(
                "get_file",
                "Read a file from a running child (e.g. its result file). Max 256 KiB.",
                json!({"type": "object", "properties": {"handle": {"type": "string"}, "path": {"type": "string"}}, "required": ["handle", "path"]}),
            ));
            tools.push(tool("stop_sandbox", "Stop a child (its children are stopped first).", handle));
            tools.push(tool("list_children", "Your children.", json!({"type": "object", "properties": {}})));
            tools.push(tool("get_limits", "Your depth, children and total left.", json!({"type": "object", "properties": {}})));
        }
        Principal::User(_) => {
            tools.push(tool("grant_list", "List grants (user only).", json!({"type": "object", "properties": {}})));
            tools.push(tool(
                "grant_update",
                "Widen, shrink or revoke a grant (user only). Affects new creates only.",
                json!({"type": "object", "properties": {
                    "grant_id": {"type": "string"},
                    "max_depth": {"type": "integer"}, "max_breadth": {"type": "integer"},
                    "max_total": {"type": "integer"}, "revoke": {"type": "boolean"}
                }, "required": ["grant_id"]}),
            ));
        }
        _ => {}
    }
    tools
}

/// Why `sandbox_id` may not create a child, if it may not.
fn can_create(state: &ServerState, sandbox_id: &str) -> Result<(Lineage, Grant), ToolError> {
    let lineage = state
        .mcp
        .lineage(sandbox_id)
        .ok_or_else(|| terr("NOT_ALLOWED", "this sandbox has no grant"))?;
    let grant = state
        .mcp
        .grant(&lineage.grant_id)
        .ok_or_else(|| terr("NOT_ALLOWED", "grant revoked"))?;
    if now_ms() > grant.expires_at_ms {
        return Err(terr("NOT_ALLOWED", "grant expired"));
    }
    if lineage.depth >= grant.max_depth {
        return Err(terr("LIMIT_REACHED", format!("depth: {} of {}", lineage.depth, grant.max_depth)));
    }
    if lineage.children_created >= grant.max_breadth {
        return Err(terr("LIMIT_REACHED", format!("breadth: {} of {}", lineage.children_created, grant.max_breadth)));
    }
    if grant.total >= grant.max_total {
        return Err(terr("LIMIT_REACHED", format!("total: {} of {}", grant.total, grant.max_total)));
    }
    Ok((lineage, grant))
}

async fn call_tool(
    state: &Arc<ServerState>,
    principal: &Principal,
    name: &str,
    args: &Value,
) -> Result<Value, ToolError> {
    match (principal, name) {
        (Principal::Sandbox(s), "create_sandbox") => create_child(state, &s.sandbox_id, args).await,
        (Principal::Sandbox(s), "wait") => wait_child(state, &s.sandbox_id, args).await,
        (Principal::Sandbox(s), "get_output") => child_output(state, &s.sandbox_id, args).await,
        (Principal::Sandbox(s), "get_file") => child_file(state, &s.sandbox_id, args).await,
        (Principal::Sandbox(s), "stop_sandbox") => stop_child(state, &s.sandbox_id, args).await,
        (Principal::Sandbox(s), "list_children") => Ok(list_children(state, &s.sandbox_id)),
        (Principal::Sandbox(s), "get_limits") => Ok(get_limits(state, &s.sandbox_id)),
        (Principal::User(_), "grant_list") => Ok(grant_list(state)),
        (Principal::User(u), "grant_update") => grant_update(state, u, args),
        (Principal::Sandbox(_), "grant_update" | "grant_list") => {
            Err(terr("NOT_ALLOWED", "only the user can change grants"))
        }
        _ => Err(terr("NOT_FOUND", format!("unknown tool '{name}'"))),
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Prove `candidate` fits inside `ceiling`. Only `Within` passes.
async fn prove(ceiling: &SandboxPolicy, candidate: &SandboxPolicy) -> Result<(), ToolError> {
    let ser = |p: &SandboxPolicy| {
        openshell_policy::serialize_sandbox_policy(p)
            .map_err(|e| terr("INVALID_POLICY", e.to_string()))
            .and_then(|y| {
                containment::parse_policy_str(&y).map_err(|e| terr("INVALID_POLICY", e.to_string()))
            })
    };
    let boundary = ser(ceiling)?;
    let child = ser(candidate)?;
    let result = tokio::task::spawn_blocking(move || {
        containment::check_within_boundary(&boundary, &child, CheckOptions::new(PROVER_TIMEOUT))
    })
    .await
    .map_err(|e| terr("INTERNAL", e.to_string()))?;
    match result {
        CheckResult::Within(_) => Ok(()),
        CheckResult::Exceeds(ev) => Err(terr(
            "EXCEEDS_CEILING",
            format!("child policy exceeds the ceiling; counterexample: {:?}", ev.counterexample()),
        )),
        CheckResult::Unsupported(ev) => Err(terr("UNSUPPORTED_RULES", ev.reason().to_string())),
        CheckResult::Inconclusive(ev) => Err(terr("PROVER_INCONCLUSIVE", ev.reason().to_string())),
    }
}

async fn create_child(
    state: &Arc<ServerState>,
    caller_id: &str,
    args: &Value,
) -> Result<Value, ToolError> {
    // 1. Retry: same request_id returns the first result, counted once.
    let request_id = str_arg(args, "request_id").map(str::to_string);
    if let (Some(rid), Some(l)) = (&request_id, state.mcp.lineage(caller_id))
        && let Some(g) = state.mcp.grant(&l.grant_id)
        && let Some(name) = g.request_ids.get(rid)
    {
        return Ok(json!({"handle": name, "repeated_request": true}));
    }
    // 2. Global policy active: the child's policy would be thrown away.
    if crate::grpc::policy::global_policy_active(state)
        .await
        .map_err(|s| terr("INTERNAL", s.message()))?
    {
        return Err(terr("GLOBAL_POLICY_ACTIVE", "a global policy replaces sandbox policies on this gateway"));
    }
    // 3. Grant and limits.
    let (parent_lineage, grant) = can_create(state, caller_id)?;
    let parent = state
        .store
        .get_message::<Sandbox>(caller_id)
        .await
        .map_err(|e| terr("INTERNAL", e.to_string()))?
        .ok_or_else(|| terr("NOT_FOUND", "caller sandbox not found"))?;
    let parent_spec = parent.spec.clone().unwrap_or_default();

    // 4. Child policy = ceiling's non-network parts + the agent's network rules.
    let mut candidate = grant.ceiling.clone();
    candidate.network_policies.clear();
    if let Some(p) = args.get("policy").filter(|p| !p.is_null()) {
        let mut p = p.clone();
        if let Some(obj) = p.as_object_mut() {
            obj.entry("version").or_insert(json!(1));
        }
        let parsed = openshell_policy::parse_sandbox_policy(&p.to_string())
            .map_err(|e| terr("INVALID_POLICY", e.to_string()))?;
        candidate.network_policies = parsed.network_policies;
    }
    if candidate.network_policies.contains_key(DOOR_RULE_KEY) {
        return Err(terr("INVALID_POLICY", "the door rule is written by the gateway"));
    }
    // 5. Prove against the ceiling (not the parent's current policy).
    prove(&grant.ceiling, &candidate).await?;

    // 6. Providers must be a subset of the parent's.
    let providers: Vec<String> = match args.get("providers").and_then(Value::as_array) {
        Some(list) => list.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => parent_spec.providers.clone(),
    };
    if let Some(extra) = providers.iter().find(|p| !parent_spec.providers.contains(p)) {
        return Err(terr("NOT_ALLOWED", format!("provider '{extra}' is not attached to you")));
    }

    // 7. Reserve counters atomically.
    let depth = parent_lineage.depth + 1;
    {
        let mut inner = state.mcp.lock();
        let g = inner.grants.get_mut(&grant.id).ok_or_else(|| terr("NOT_ALLOWED", "grant revoked"))?;
        if g.total >= g.max_total {
            return Err(terr("LIMIT_REACHED", "total"));
        }
        g.total += 1;
        if let Some(l) = inner.lineage.get_mut(caller_id) {
            l.children_created += 1;
        }
    }
    let rollback = |state: &ServerState| {
        let mut inner = state.mcp.lock();
        if let Some(g) = inner.grants.get_mut(&grant.id) {
            g.total = g.total.saturating_sub(1);
        }
        if let Some(l) = inner.lineage.get_mut(caller_id) {
            l.children_created = l.children_created.saturating_sub(1);
        }
    };

    // 8. Door rule only if the child may create children itself.
    let mut policy = candidate;
    if depth < grant.max_depth {
        let rule = door_rule(&grant.program).map_err(|e| terr("INTERNAL", e))?;
        policy.network_policies.insert(DOOR_RULE_KEY.to_string(), rule);
    }

    // 9. Same image, CPU and memory as the parent; no GPU.
    let mut template = parent_spec.template.clone().unwrap_or_default();
    template.driver_config = None;
    let command: Vec<String> = args
        .get("command")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    if command.is_empty() {
        rollback(state);
        return Err(terr("INVALID_ARGUMENT", "command is required"));
    }
    let mut environment = HashMap::new();
    if let Some(task) = str_arg(args, "task") {
        environment.insert("OPENSHELL_TASK".to_string(), task.to_string());
    }
    let kind = str_arg(args, "kind").unwrap_or("agent").to_string();
    let lifetime = str_arg(args, "lifetime").unwrap_or("until_stopped").to_string();
    let name = str_arg(args, "name").map_or_else(
        || format!("{}-c{}", parent.object_name(), parent_lineage.children_created + 1),
        str::to_string,
    );

    let spec = SandboxSpec {
        environment,
        template: Some(template),
        policy: Some(policy),
        providers,
        command,
        ..Default::default()
    };
    let mut req = tonic::Request::new(CreateSandboxRequest {
        workspace_scope: Some(workspace_selector(parent.object_workspace().to_string())),
        spec: Some(spec),
        name: name.clone(),
        ..Default::default()
    });
    // Acts for the user who set the grant (root user).
    req.extensions_mut()
        .insert(Principal::User(UserPrincipal { identity: grant.user.clone() }));
    let created = match crate::grpc::sandbox::handle_create_sandbox(state, req).await {
        Ok(resp) => resp.into_inner().sandbox.unwrap_or_default(),
        Err(status) => {
            rollback(state);
            return Err(terr("CREATE_FAILED", status.message().to_string()));
        }
    };

    // 10. Record lineage and the request_id.
    {
        let mut inner = state.mcp.lock();
        inner.lineage.insert(
            created.object_id().to_string(),
            Lineage {
                sandbox_id: created.object_id().to_string(),
                name: created.object_name().to_string(),
                workspace: created.object_workspace().to_string(),
                parent_id: Some(caller_id.to_string()),
                depth,
                grant_id: grant.id.clone(),
                kind: kind.clone(),
                lifetime: lifetime.clone(),
                children_created: 0,
            },
        );
        if let (Some(rid), Some(g)) = (request_id, inner.grants.get_mut(&grant.id)) {
            g.request_ids.insert(rid, created.object_name().to_string());
        }
    }
    info!(parent = %parent.object_name(), child = %created.object_name(), depth, "mcp: child created");
    Ok(json!({
        "handle": created.object_name(),
        "depth": depth,
        "can_create_children": depth < grant.max_depth,
        "kind": kind, "lifetime": lifetime
    }))
}

/// A child of the caller (any depth below it), by name.
fn own_child(state: &ServerState, caller_id: &str, args: &Value) -> Result<Lineage, ToolError> {
    let handle = str_arg(args, "handle").ok_or_else(|| terr("INVALID_ARGUMENT", "handle is required"))?;
    state
        .mcp
        .descendants_deepest_first(caller_id)
        .into_iter()
        .find(|l| l.name == handle)
        .ok_or_else(|| terr("NOT_YOUR_CHILD", format!("'{handle}' is not your child")))
}

async fn wait_child(state: &Arc<ServerState>, caller_id: &str, args: &Value) -> Result<Value, ToolError> {
    let child = own_child(state, caller_id, args)?;
    let timeout = args
        .get("timeout_s")
        .and_then(Value::as_u64)
        .map_or(Duration::from_secs(120), Duration::from_secs)
        .min(MAX_WAIT);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let sb = state
            .store
            .get_message::<Sandbox>(&child.sandbox_id)
            .await
            .map_err(|e| terr("INTERNAL", e.to_string()))?;
        let Some(sb) = sb else {
            return Ok(json!({"handle": child.name, "status": "gone"}));
        };
        let status = sb.status.unwrap_or_default();
        let phase = SandboxPhase::try_from(status.phase).unwrap_or(SandboxPhase::Unspecified);
        let exited = status.exit_code.is_some()
            || matches!(phase, SandboxPhase::Completed | SandboxPhase::Error | SandboxPhase::Stopped);
        let ready = phase == SandboxPhase::Ready && child.lifetime == "until_stopped" && args.get("until_exit").is_none();
        if exited || ready || tokio::time::Instant::now() >= deadline {
            return Ok(json!({
                "handle": child.name,
                "phase": phase.as_str_name(),
                "exit_code": status.exit_code,
                "timed_out": !exited && !ready
            }));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn child_output(state: &Arc<ServerState>, caller_id: &str, args: &Value) -> Result<Value, ToolError> {
    let child = own_child(state, caller_id, args)?;
    let grant = state.mcp.grant(&child.grant_id).ok_or_else(|| terr("NOT_ALLOWED", "grant revoked"))?;
    let mut req = tonic::Request::new(GetSandboxLogsRequest {
        workspace_scope: Some(workspace_selector(child.workspace.clone())),
        sandbox: child.name.clone(),
        lines: 200,
        ..Default::default()
    });
    req.extensions_mut()
        .insert(Principal::User(UserPrincipal { identity: grant.user.clone() }));
    let logs = crate::grpc::policy::handle_get_sandbox_logs(state, req)
        .await
        .map_err(|s| terr("INTERNAL", s.message()))?
        .into_inner();
    let lines: Vec<String> = logs
        .logs
        .iter()
        .map(|l| format!("[{}] {}", l.source, l.message))
        .collect();
    Ok(json!({"handle": child.name, "lines": lines}))
}

const MAX_FILE_BYTES: usize = 256 * 1024;

async fn child_file(state: &Arc<ServerState>, caller_id: &str, args: &Value) -> Result<Value, ToolError> {
    use openshell_core::proto::{ExecSandboxRequest, exec_sandbox_event::Payload};
    use tokio_stream::StreamExt;
    let child = own_child(state, caller_id, args)?;
    let path = str_arg(args, "path").ok_or_else(|| terr("INVALID_ARGUMENT", "path is required"))?;
    let grant = state.mcp.grant(&child.grant_id).ok_or_else(|| terr("NOT_ALLOWED", "grant revoked"))?;
    let mut req = tonic::Request::new(ExecSandboxRequest {
        workspace_scope: Some(workspace_selector(child.workspace.clone())),
        sandbox: child.name.clone(),
        command: vec!["cat".to_string(), path.to_string()],
        no_login_shell: true,
        ..Default::default()
    });
    req.extensions_mut()
        .insert(Principal::User(UserPrincipal { identity: grant.user.clone() }));
    let mut stream = crate::grpc::sandbox::handle_exec_sandbox(state, req)
        .await
        .map_err(|s| terr("EXEC_FAILED", s.message()))?
        .into_inner();
    let (mut out, mut err, mut code) = (Vec::new(), Vec::new(), None);
    while let Some(ev) = stream.next().await {
        match ev.map_err(|s| terr("EXEC_FAILED", s.message()))?.payload {
            Some(Payload::Stdout(o)) => out.extend_from_slice(&o.data),
            Some(Payload::Stderr(e)) => err.extend_from_slice(&e.data),
            Some(Payload::Exit(x)) => code = Some(x.exit_code),
            None => {}
        }
        if out.len() > MAX_FILE_BYTES {
            out.truncate(MAX_FILE_BYTES);
            break;
        }
    }
    if code.unwrap_or(0) != 0 {
        return Err(terr("FILE_ERROR", String::from_utf8_lossy(&err).trim().to_string()));
    }
    Ok(json!({"handle": child.name, "path": path, "content": String::from_utf8_lossy(&out)}))
}

async fn stop_child(state: &Arc<ServerState>, caller_id: &str, args: &Value) -> Result<Value, ToolError> {
    let child = own_child(state, caller_id, args)?;
    cascade_stop_children(state, &child.sandbox_id).await;
    state
        .compute
        .delete_sandbox_by_id(&child.sandbox_id, &child.name)
        .await
        .map_err(|s| terr("INTERNAL", s.message()))?;
    state.mcp.forget(&child.sandbox_id);
    Ok(json!({"handle": child.name, "stopped": true}))
}

fn list_children(state: &ServerState, caller_id: &str) -> Value {
    let kids: Vec<Value> = state
        .mcp
        .descendants_deepest_first(caller_id)
        .into_iter()
        .filter(|l| l.parent_id.as_deref() == Some(caller_id))
        .map(|l| json!({"handle": l.name, "kind": l.kind, "lifetime": l.lifetime, "depth": l.depth}))
        .collect();
    json!({"children": kids})
}

fn get_limits(state: &ServerState, caller_id: &str) -> Value {
    let Some(l) = state.mcp.lineage(caller_id) else {
        return json!({"has_grant": false, "can_create_children": false});
    };
    let Some(g) = state.mcp.grant(&l.grant_id) else {
        return json!({"has_grant": false, "can_create_children": false});
    };
    json!({
        "has_grant": true,
        "grant_id": g.id,
        "depth": l.depth, "max_depth": g.max_depth,
        "children_created": l.children_created, "max_breadth": g.max_breadth,
        "total": g.total, "max_total": g.max_total,
        "can_create_children": can_create(state, caller_id).is_ok()
    })
}

fn grant_list(state: &ServerState) -> Value {
    let inner = state.mcp.lock();
    let grants: Vec<Value> = inner
        .grants
        .values()
        .map(|g| {
            let holder = inner.lineage.get(&g.holder_id).map(|l| l.name.clone()).unwrap_or_default();
            json!({"grant_id": g.id, "holder": holder, "max_depth": g.max_depth,
                   "max_breadth": g.max_breadth, "max_total": g.max_total, "total": g.total,
                   "program": g.program, "user": g.user.subject})
        })
        .collect();
    json!({"grants": grants})
}

fn grant_update(state: &ServerState, user: &UserPrincipal, args: &Value) -> Result<Value, ToolError> {
    let id = str_arg(args, "grant_id").ok_or_else(|| terr("INVALID_ARGUMENT", "grant_id is required"))?;
    let mut inner = state.mcp.lock();
    if args.get("revoke").and_then(Value::as_bool) == Some(true) {
        inner.grants.remove(id).ok_or_else(|| terr("NOT_FOUND", "no such grant"))?;
        info!(grant_id = %id, user = %user.identity.subject, "mcp: grant revoked");
        return Ok(json!({"grant_id": id, "revoked": true}));
    }
    let g = inner.grants.get_mut(id).ok_or_else(|| terr("NOT_FOUND", "no such grant"))?;
    if g.user.subject != user.identity.subject {
        return Err(terr("NOT_ALLOWED", "only the user who set the grant can change it"));
    }
    let set = |v: &mut u32, key: &str| {
        if let Some(n) = args.get(key).and_then(Value::as_u64) {
            *v = u32::try_from(n).unwrap_or(u32::MAX);
        }
    };
    set(&mut g.max_depth, "max_depth");
    set(&mut g.max_breadth, "max_breadth");
    set(&mut g.max_total, "max_total");
    info!(grant_id = %id, user = %user.identity.subject, ?args, "mcp: grant updated (affects new creates only)");
    Ok(json!({"grant_id": id, "max_depth": g.max_depth, "max_breadth": g.max_breadth, "max_total": g.max_total}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_grant_spec() {
        let g = parse_grant_spec("max_depth=1,max_breadth=2,max_total=3,expires=4h,program=/usr/local/bin/node").unwrap();
        assert_eq!(g.max_depth, 1);
        assert_eq!(g.max_breadth, 2);
        assert_eq!(g.max_total, 3);
        assert_eq!(g.expires, Duration::from_secs(4 * 3600));
        assert_eq!(g.program, "/usr/local/bin/node");
    }

    #[test]
    fn grant_needs_absolute_program() {
        assert!(parse_grant_spec("max_depth=1").is_err());
        assert!(parse_grant_spec("program=node").is_err());
        assert!(parse_grant_spec("bogus=1,program=/x").is_err());
    }

    #[test]
    fn door_rule_names_host_port_and_program() {
        let r = door_rule("/usr/local/bin/node").unwrap();
        assert_eq!(r.endpoints[0].host, DOOR_HOST);
        assert_eq!(r.endpoints[0].port, DOOR_PORT);
        assert_eq!(r.binaries[0].path, "/usr/local/bin/node");
    }

    #[test]
    fn descendants_are_deepest_first() {
        let s = McpState::default();
        let mk = |id: &str, parent: Option<&str>, depth| Lineage {
            sandbox_id: id.into(), name: id.into(), workspace: "w".into(),
            parent_id: parent.map(str::to_string), depth, grant_id: "g".into(),
            kind: "agent".into(), lifetime: "until_stopped".into(), children_created: 0,
        };
        {
            let mut i = s.lock();
            i.lineage.insert("lead".into(), mk("lead", None, 0));
            i.lineage.insert("a".into(), mk("a", Some("lead"), 1));
            i.lineage.insert("a1".into(), mk("a1", Some("a"), 2));
        }
        let d: Vec<String> = s.descendants_deepest_first("lead").into_iter().map(|l| l.name).collect();
        assert_eq!(d, vec!["a1".to_string(), "a".to_string()]);
    }
}
