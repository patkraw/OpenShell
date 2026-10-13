// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Child sandboxes: grants, lineage, policy proof and lifecycle, and the
//! gateway's implementation of the `openshell-gateway-mcp` backend
//! (proof of concept for the "Gateway MCP for creating sandboxes" RFC draft).
//!
//! The MCP protocol itself lives in the `openshell-gateway-mcp` crate; this
//! module only decides.
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
use crate::auth::identity::Identity;
use crate::auth::principal::{Principal, UserPrincipal};
use axum::http::HeaderMap;
use openshell_core::proto::{
    CreateSandboxRequest, GetSandboxLogsRequest, Sandbox, SandboxPhase, SandboxPolicy, SandboxSpec,
    SandboxTemplate, workspace_selector,
};
use openshell_core::{ObjectId, ObjectName, ObjectWorkspace};
use openshell_gateway_mcp::{
    Access, ApiError, CallerInfo, CreateSandboxArgs, Operation, SandboxApi,
};
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

pub const KIND_AGENT: &str = "agent";
pub const KIND_TOOL: &str = "tool";
/// Default idle timeout for tool sandboxes.
const DEFAULT_TOOL_IDLE: Duration = Duration::from_secs(600);
const REAPER_INTERVAL: Duration = Duration::from_secs(5);
const MAX_EXEC_TIMEOUT: Duration = Duration::from_secs(300);

/// Whether a new child gets the door rule: only agents, and only with depth left.
/// Tool sandboxes run commands for their parent and never create children.
pub fn gets_door_rule(kind: &str, depth: u32, max_depth: u32) -> bool {
    kind == KIND_AGENT && depth < max_depth
}

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
    /// Children get their parent's ceiling unchanged; agents may not write child policies.
    pub inherit_child_policy: bool,
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
    /// Tool sandboxes: stopped after this long without an `exec` / `get_file`.
    pub idle_timeout_ms: Option<i64>,
    pub last_used_ms: i64,
}

#[derive(Debug, Default)]
struct Inner {
    grants: HashMap<String, Grant>,
    lineage: HashMap<String, Lineage>,
}

#[derive(Debug, Default)]
pub struct ChildrenState {
    inner: Mutex<Inner>,
    reaper_started: std::sync::atomic::AtomicBool,
}

impl ChildrenState {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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

    fn touch(&self, sandbox_id: &str) {
        if let Some(l) = self.lock().lineage.get_mut(sandbox_id) {
            l.last_used_ms = now_ms();
        }
    }

    /// Tool sandboxes idle past their timeout.
    fn idle_tools(&self, now: i64) -> Vec<Lineage> {
        self.lock()
            .lineage
            .values()
            .filter(|l| l.kind == KIND_TOOL)
            .filter(|l| l.idle_timeout_ms.is_some_and(|t| now - l.last_used_ms > t))
            .cloned()
            .collect()
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
    /// `child_policy=inherit`: every child gets its parent's ceiling unchanged.
    pub inherit_child_policy: bool,
}

pub fn parse_grant_spec(spec: &str) -> Result<GrantSpec, String> {
    let mut out = GrantSpec {
        max_depth: 1,
        max_breadth: 2,
        max_total: 3,
        expires: Duration::from_secs(4 * 3600),
        program: String::new(),
        inherit_child_policy: false,
    };
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| format!("grant: expected key=value, got '{part}'"))?;
        let num = || {
            v.parse::<u32>()
                .map_err(|_| format!("grant: '{k}' must be a number"))
        };
        match k {
            "max_depth" => out.max_depth = num()?,
            "max_breadth" => out.max_breadth = num()?,
            "max_total" => out.max_total = num()?,
            "expires" => out.expires = parse_duration(v)?,
            "program" => out.program = v.to_string(),
            "child_policy" => {
                out.inherit_child_policy = match v {
                    "inherit" => true,
                    "narrow" => false,
                    _ => return Err("grant: child_policy must be 'narrow' or 'inherit'".into()),
                }
            }
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
    let n: u64 = n
        .parse()
        .map_err(|_| format!("grant: bad duration '{v}'"))?;
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
        return Err(tonic::Status::permission_denied(
            "only a user can set a grant",
        ));
    }
    let grant = parse_grant_spec(raw).map_err(tonic::Status::invalid_argument)?;
    let policy = spec.policy.get_or_insert_with(SandboxPolicy::default);
    let ceiling = policy.clone();
    if grant.max_depth > 0 {
        let rule = door_rule(&grant.program).map_err(tonic::Status::internal)?;
        policy
            .network_policies
            .insert(DOOR_RULE_KEY.to_string(), rule);
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
    let mut inner = state.children.lock();
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
            inherit_child_policy: grant.inherit_child_policy,
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
            kind: KIND_AGENT.into(),
            lifetime: "until_stopped".into(),
            children_created: 0,
            idle_timeout_ms: None,
            last_used_ms: now_ms(),
        },
    );
    info!(grant_id = %grant_id, holder = %sandbox.object_name(), "mcp: grant registered");
}

/// Stop a sandbox's children, deepest first, before the sandbox itself.
pub async fn cascade_stop_children(state: &Arc<ServerState>, sandbox_id: &str) {
    for child in state.children.descendants_deepest_first(sandbox_id) {
        info!(child = %child.name, parent = ?child.parent_id, "mcp: cascade stop");
        if let Err(e) = state
            .compute
            .delete_sandbox_by_id(&child.sandbox_id, &child.name)
            .await
        {
            warn!(child = %child.name, error = %e, "mcp: cascade delete failed");
        }
        state.children.forget(&child.sandbox_id);
    }
}

// ---------------------------------------------------------------------------
// HTTP / JSON-RPC
// ---------------------------------------------------------------------------

async fn authenticate(state: &ServerState, headers: &HeaderMap) -> Result<Principal, String> {
    if let Some(chain) = crate::multiplex::build_authenticator_chain(state) {
        match chain.authenticate(headers, "/mcp").await {
            Ok(Some(p)) => return Ok(p),
            Ok(None) => {}
            Err(status) => return Err(status.message().to_string()),
        }
    }
    if state.config.auth.allow_unauthenticated_users {
        return Ok(crate::multiplex::unauthenticated_dev_user_principal());
    }
    Err("unauthenticated".into())
}

fn caller_label(p: &Principal) -> String {
    match p {
        Principal::User(u) => format!("user:{}", u.identity.subject),
        Principal::Sandbox(s) => format!("sandbox:{}", s.sandbox_id),
        Principal::Peer(_) => "peer".into(),
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
    ToolError {
        code,
        message: message.into(),
    }
}

/// Why `sandbox_id` may not create a child, if it may not.
fn can_create(state: &ServerState, sandbox_id: &str) -> Result<(Lineage, Grant), ToolError> {
    let lineage = state
        .children
        .lineage(sandbox_id)
        .ok_or_else(|| terr("NOT_ALLOWED", "this sandbox has no grant"))?;
    if lineage.kind == KIND_TOOL {
        return Err(terr("NOT_ALLOWED", "tool sandboxes cannot create children"));
    }
    let grant = state
        .children
        .grant(&lineage.grant_id)
        .ok_or_else(|| terr("NOT_ALLOWED", "grant revoked"))?;
    if now_ms() > grant.expires_at_ms {
        return Err(terr("NOT_ALLOWED", "grant expired"));
    }
    if lineage.depth >= grant.max_depth {
        return Err(terr(
            "LIMIT_REACHED",
            format!("depth: {} of {}", lineage.depth, grant.max_depth),
        ));
    }
    if lineage.children_created >= grant.max_breadth {
        return Err(terr(
            "LIMIT_REACHED",
            format!(
                "breadth: {} of {}",
                lineage.children_created, grant.max_breadth
            ),
        ));
    }
    if grant.total >= grant.max_total {
        return Err(terr(
            "LIMIT_REACHED",
            format!("total: {} of {}", grant.total, grant.max_total),
        ));
    }
    Ok((lineage, grant))
}

impl CallerInfo for Principal {
    fn label(&self) -> String {
        caller_label(self)
    }
}

impl From<ToolError> for ApiError {
    fn from(e: ToolError) -> Self {
        ApiError::new(e.code, e.message)
    }
}

fn val<T: serde::Serialize>(a: &T) -> Value {
    serde_json::to_value(a).unwrap_or(Value::Null)
}

/// The gateway's backend for the MCP surface. Every rule is enforced here.
pub struct GatewaySandboxApi {
    state: Arc<ServerState>,
}

impl GatewaySandboxApi {
    pub fn new(state: Arc<ServerState>) -> Self {
        Self { state }
    }
}

#[async_trait::async_trait]
impl SandboxApi for GatewaySandboxApi {
    type Caller = Principal;

    async fn authenticate(&self, headers: &HeaderMap) -> Result<Principal, ApiError> {
        authenticate(&self.state, headers)
            .await
            .map_err(|m| ApiError::new("UNAUTHENTICATED", m))
    }

    /// The gateway hides `create_sandbox` from sandboxes that cannot use it.
    async fn access(&self, caller: &Principal) -> Access {
        match caller {
            Principal::Sandbox(s) => Access {
                create_sandbox: can_create(&self.state, &s.sandbox_id).is_ok(),
                manage_children: true,
                grants: false,
            },
            Principal::User(_) => Access {
                create_sandbox: true,
                manage_children: false,
                grants: true,
            },
            _ => Access::default(),
        }
    }

    async fn call(&self, caller: &Principal, op: Operation) -> Result<Value, ApiError> {
        let state = &self.state;
        let result = match (caller, op) {
            (Principal::Sandbox(s), Operation::CreateSandbox(a)) => {
                create_child(state, &s.sandbox_id, &val(&a)).await
            }
            (Principal::User(u), Operation::CreateSandbox(a)) => {
                create_top_level(state, u, a).await
            }
            (Principal::Sandbox(s), Operation::Wait(a)) => {
                wait_child(state, &s.sandbox_id, &val(&a)).await
            }
            (Principal::Sandbox(s), Operation::Exec(a)) => {
                child_exec(state, &s.sandbox_id, &val(&a)).await
            }
            (Principal::Sandbox(s), Operation::GetFile(a)) => {
                child_file(state, &s.sandbox_id, &val(&a)).await
            }
            (Principal::Sandbox(s), Operation::GetOutput(a)) => {
                child_output(state, &s.sandbox_id, &val(&a)).await
            }
            (Principal::Sandbox(s), Operation::Stop(a)) => {
                stop_child(state, &s.sandbox_id, &val(&a)).await
            }
            (Principal::Sandbox(s), Operation::ListChildren) => {
                Ok(list_children(state, &s.sandbox_id))
            }
            (Principal::Sandbox(s), Operation::GetLimits) => Ok(get_limits(state, &s.sandbox_id)),
            (Principal::User(_), Operation::GrantList) => Ok(grant_list(state)),
            (Principal::User(u), Operation::GrantUpdate(a)) => grant_update(state, u, &val(&a)),
            (_, Operation::GrantList | Operation::GrantUpdate(_)) => {
                Err(terr("NOT_ALLOWED", "only the user can change grants"))
            }
            _ => Err(terr("NOT_ALLOWED", "not available to this caller")),
        };
        result.map_err(ApiError::from)
    }
}

/// A user (an agent on the host acting with the user's login) creates a
/// top-level sandbox, exactly as `openshell sandbox create` would, optionally
/// with a grant so that the new sandbox can create children.
async fn create_top_level(
    state: &Arc<ServerState>,
    user: &UserPrincipal,
    a: CreateSandboxArgs,
) -> Result<Value, ToolError> {
    let mut annotations = HashMap::new();
    let has_grant = a.grant.as_deref().is_some_and(|g| !g.is_empty());
    if let Some(g) = a.grant.filter(|g| !g.is_empty()) {
        annotations.insert(GRANT_ANNOTATION.to_string(), g);
    }
    let policy = match a.policy {
        Some(mut p) => {
            if let Some(obj) = p.as_object_mut() {
                obj.entry("version").or_insert(json!(1));
            }
            Some(
                openshell_policy::parse_sandbox_policy(&p.to_string())
                    .map_err(|e| terr("INVALID_POLICY", e.to_string()))?,
            )
        }
        None => None,
    };
    let mut environment = HashMap::new();
    if let Some(task) = a.task.filter(|t| !t.is_empty()) {
        environment.insert("OPENSHELL_TASK".to_string(), task);
    }
    let template = SandboxTemplate {
        image: a.image.unwrap_or_default(),
        ..Default::default()
    };
    let spec = SandboxSpec {
        environment,
        template: Some(template),
        policy,
        providers: a.providers.unwrap_or_default(),
        command: a.command,
        ..Default::default()
    };
    let mut req = tonic::Request::new(CreateSandboxRequest {
        workspace_scope: Some(workspace_selector("default".to_string())),
        spec: Some(spec),
        name: a.name.unwrap_or_default(),
        annotations,
        ..Default::default()
    });
    req.extensions_mut().insert(Principal::User(user.clone()));
    let created = crate::grpc::sandbox::handle_create_sandbox(state, req)
        .await
        .map_err(|s| terr("CREATE_FAILED", s.message().to_string()))?
        .into_inner()
        .sandbox
        .unwrap_or_default();
    info!(user = %user.identity.subject, sandbox = %created.object_name(), has_grant, "children: top-level sandbox created");
    Ok(json!({"handle": created.object_name(), "has_grant": has_grant}))
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
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
            format!(
                "child policy exceeds the ceiling; counterexample: {:?}",
                ev.counterexample()
            ),
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
    if let (Some(rid), Some(l)) = (&request_id, state.children.lineage(caller_id))
        && let Some(g) = state.children.grant(&l.grant_id)
        && let Some(name) = g.request_ids.get(rid)
    {
        return Ok(json!({"handle": name, "repeated_request": true}));
    }
    // 2. Global policy active: the child's policy would be thrown away.
    if crate::grpc::policy::global_policy_active(state)
        .await
        .map_err(|s| terr("INTERNAL", s.message()))?
    {
        return Err(terr(
            "GLOBAL_POLICY_ACTIVE",
            "a global policy replaces sandbox policies on this gateway",
        ));
    }
    // 3. Grant and limits.
    let (parent_lineage, grant) = can_create(state, caller_id)?;
    let kind = str_arg(args, "kind").unwrap_or(KIND_AGENT).to_string();
    if kind != KIND_AGENT && kind != KIND_TOOL {
        return Err(terr(
            "INVALID_ARGUMENT",
            "kind must be \"agent\" or \"tool\"",
        ));
    }
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
    let wants_policy = args.get("policy").is_some_and(|p| !p.is_null());
    if grant.inherit_child_policy {
        if wants_policy {
            return Err(terr(
                "INVALID_ARGUMENT",
                "this tree's grant gives every child its parent's ceiling; omit policy",
            ));
        }
        candidate.network_policies = grant.ceiling.network_policies.clone();
    }
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
        return Err(terr(
            "INVALID_POLICY",
            "the door rule is written by the gateway",
        ));
    }
    // 5. Prove against the ceiling (not the parent's current policy).
    prove(&grant.ceiling, &candidate).await?;

    // 6. Providers must be a subset of the parent's.
    let providers: Vec<String> = match args.get("providers").and_then(Value::as_array) {
        Some(list) => list
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        None => parent_spec.providers.clone(),
    };
    if let Some(extra) = providers
        .iter()
        .find(|p| !parent_spec.providers.contains(p))
    {
        return Err(terr(
            "NOT_ALLOWED",
            format!("provider '{extra}' is not attached to you"),
        ));
    }

    // 7. Reserve counters atomically.
    let depth = parent_lineage.depth + 1;
    {
        let mut inner = state.children.lock();
        let g = inner
            .grants
            .get_mut(&grant.id)
            .ok_or_else(|| terr("NOT_ALLOWED", "grant revoked"))?;
        if g.total >= g.max_total {
            return Err(terr("LIMIT_REACHED", "total"));
        }
        g.total += 1;
        if let Some(l) = inner.lineage.get_mut(caller_id) {
            l.children_created += 1;
        }
    }
    let rollback = |state: &ServerState| {
        let mut inner = state.children.lock();
        if let Some(g) = inner.grants.get_mut(&grant.id) {
            g.total = g.total.saturating_sub(1);
        }
        if let Some(l) = inner.lineage.get_mut(caller_id) {
            l.children_created = l.children_created.saturating_sub(1);
        }
    };

    // 8. Door rule only for agents that may create children themselves.
    let mut policy = candidate;
    if gets_door_rule(&kind, depth, grant.max_depth) {
        let rule = door_rule(&grant.program).map_err(|e| terr("INTERNAL", e))?;
        policy
            .network_policies
            .insert(DOOR_RULE_KEY.to_string(), rule);
    }

    // 9. Same image, CPU and memory as the parent; no GPU.
    let mut template = parent_spec.template.clone().unwrap_or_default();
    template.driver_config = None;
    let mut command: Vec<String> = args
        .get("command")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if command.is_empty() && kind == KIND_TOOL {
        // A tool sandbox just stays up and runs what its parent sends with `exec`.
        command = vec!["sleep".into(), "infinity".into()];
    }
    if command.is_empty() {
        rollback(state);
        return Err(terr("INVALID_ARGUMENT", "command is required"));
    }
    let mut environment = HashMap::new();
    if let Some(task) = str_arg(args, "task") {
        environment.insert("OPENSHELL_TASK".to_string(), task.to_string());
    }
    let idle_timeout_ms = (kind == KIND_TOOL).then(|| {
        let secs = args
            .get("idle_timeout_s")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TOOL_IDLE.as_secs());
        i64::try_from(secs.saturating_mul(1000)).unwrap_or(i64::MAX)
    });
    let lifetime = str_arg(args, "lifetime")
        .unwrap_or("until_stopped")
        .to_string();
    let name = str_arg(args, "name").map_or_else(
        || {
            format!(
                "{}-c{}",
                parent.object_name(),
                parent_lineage.children_created + 1
            )
        },
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
    req.extensions_mut().insert(Principal::User(UserPrincipal {
        identity: grant.user.clone(),
    }));
    let created = match crate::grpc::sandbox::handle_create_sandbox(state, req).await {
        Ok(resp) => resp.into_inner().sandbox.unwrap_or_default(),
        Err(status) => {
            rollback(state);
            return Err(terr("CREATE_FAILED", status.message().to_string()));
        }
    };

    // 10. Record lineage and the request_id.
    {
        let mut inner = state.children.lock();
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
                idle_timeout_ms,
                last_used_ms: now_ms(),
            },
        );
        if let (Some(rid), Some(g)) = (request_id, inner.grants.get_mut(&grant.id)) {
            g.request_ids.insert(rid, created.object_name().to_string());
        }
    }
    if kind == KIND_TOOL {
        start_idle_reaper(state);
    }
    info!(parent = %parent.object_name(), child = %created.object_name(), depth, %kind, "mcp: child created");
    Ok(json!({
        "handle": created.object_name(),
        "depth": depth,
        "can_create_children": gets_door_rule(&kind, depth, grant.max_depth),
        "kind": kind, "lifetime": lifetime,
        "idle_timeout_s": idle_timeout_ms.map(|ms| ms / 1000)
    }))
}

/// A child of the caller (any depth below it), by name.
fn own_child(state: &ServerState, caller_id: &str, args: &Value) -> Result<Lineage, ToolError> {
    let handle =
        str_arg(args, "handle").ok_or_else(|| terr("INVALID_ARGUMENT", "handle is required"))?;
    state
        .children
        .descendants_deepest_first(caller_id)
        .into_iter()
        .find(|l| l.name == handle)
        .ok_or_else(|| terr("NOT_YOUR_CHILD", format!("'{handle}' is not your child")))
}

async fn wait_child(
    state: &Arc<ServerState>,
    caller_id: &str,
    args: &Value,
) -> Result<Value, ToolError> {
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
            || matches!(
                phase,
                SandboxPhase::Completed | SandboxPhase::Error | SandboxPhase::Stopped
            );
        let ready = phase == SandboxPhase::Ready
            && child.lifetime == "until_stopped"
            && args.get("until_exit").is_none();
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

async fn child_output(
    state: &Arc<ServerState>,
    caller_id: &str,
    args: &Value,
) -> Result<Value, ToolError> {
    let child = own_child(state, caller_id, args)?;
    let grant = state
        .children
        .grant(&child.grant_id)
        .ok_or_else(|| terr("NOT_ALLOWED", "grant revoked"))?;
    let mut req = tonic::Request::new(GetSandboxLogsRequest {
        workspace_scope: Some(workspace_selector(child.workspace.clone())),
        sandbox: child.name.clone(),
        lines: 200,
        ..Default::default()
    });
    req.extensions_mut().insert(Principal::User(UserPrincipal {
        identity: grant.user.clone(),
    }));
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

/// Run a command in a child through ExecSandbox, acting for the grant's user.
async fn run_in_child(
    state: &Arc<ServerState>,
    child: &Lineage,
    command: Vec<String>,
    timeout: Duration,
) -> Result<(Vec<u8>, Vec<u8>, i32), ToolError> {
    use openshell_core::proto::{ExecSandboxRequest, exec_sandbox_event::Payload};
    use tokio_stream::StreamExt;
    let grant = state
        .children
        .grant(&child.grant_id)
        .ok_or_else(|| terr("NOT_ALLOWED", "grant revoked"))?;
    let mut req = tonic::Request::new(ExecSandboxRequest {
        workspace_scope: Some(workspace_selector(child.workspace.clone())),
        sandbox: child.name.clone(),
        command,
        no_login_shell: true,
        execution_timeout: prost_types::Duration::try_from(timeout).ok(),
        ..Default::default()
    });
    req.extensions_mut().insert(Principal::User(UserPrincipal {
        identity: grant.user.clone(),
    }));
    let mut stream = crate::grpc::sandbox::handle_exec_sandbox(state, req)
        .await
        .map_err(|s| terr("EXEC_FAILED", s.message()))?
        .into_inner();
    let (mut out, mut err, mut code) = (Vec::new(), Vec::new(), 0);
    while let Some(ev) = stream.next().await {
        match ev.map_err(|s| terr("EXEC_FAILED", s.message()))?.payload {
            Some(Payload::Stdout(o)) => out.extend_from_slice(&o.data),
            Some(Payload::Stderr(e)) => err.extend_from_slice(&e.data),
            Some(Payload::Exit(x)) => code = x.exit_code,
            None => {}
        }
        out.truncate(MAX_FILE_BYTES);
        err.truncate(MAX_FILE_BYTES);
    }
    state.children.touch(&child.sandbox_id);
    Ok((out, err, code))
}

async fn child_file(
    state: &Arc<ServerState>,
    caller_id: &str,
    args: &Value,
) -> Result<Value, ToolError> {
    let child = own_child(state, caller_id, args)?;
    let path = str_arg(args, "path").ok_or_else(|| terr("INVALID_ARGUMENT", "path is required"))?;
    let (out, err, code) = run_in_child(
        state,
        &child,
        vec!["cat".into(), path.to_string()],
        Duration::from_secs(30),
    )
    .await?;
    if code != 0 {
        return Err(terr(
            "FILE_ERROR",
            String::from_utf8_lossy(&err).trim().to_string(),
        ));
    }
    Ok(json!({"handle": child.name, "path": path, "content": String::from_utf8_lossy(&out)}))
}

/// `exec` is only for tool children: an agent child runs its own task.
async fn child_exec(
    state: &Arc<ServerState>,
    caller_id: &str,
    args: &Value,
) -> Result<Value, ToolError> {
    let child = own_child(state, caller_id, args)?;
    if child.kind != KIND_TOOL {
        return Err(terr(
            "WRONG_KIND",
            format!(
                "'{}' is an agent sandbox; exec is for tool sandboxes",
                child.name
            ),
        ));
    }
    let command: Vec<String> = args
        .get("command")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if command.is_empty() {
        return Err(terr("INVALID_ARGUMENT", "command is required"));
    }
    let timeout = args
        .get("timeout_s")
        .and_then(Value::as_u64)
        .map_or(Duration::from_secs(60), Duration::from_secs)
        .min(MAX_EXEC_TIMEOUT);
    // Audit every command run in a tool sandbox (R35).
    info!(parent = %caller_id, child = %child.name, command = ?command, "mcp: tool exec");
    let (out, err, code) = run_in_child(state, &child, command, timeout).await?;
    Ok(json!({
        "handle": child.name,
        "exit_code": code,
        "stdout": String::from_utf8_lossy(&out),
        "stderr": String::from_utf8_lossy(&err)
    }))
}

/// Stop tool sandboxes that have been idle past their timeout.
fn start_idle_reaper(state: &Arc<ServerState>) {
    use std::sync::atomic::Ordering;
    if state.children.reaper_started.swap(true, Ordering::SeqCst) {
        return;
    }
    let state = Arc::clone(state);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(REAPER_INTERVAL).await;
            for idle in state.children.idle_tools(now_ms()) {
                info!(child = %idle.name, "mcp: tool sandbox idle timeout, stopping");
                if let Err(e) = state
                    .compute
                    .delete_sandbox_by_id(&idle.sandbox_id, &idle.name)
                    .await
                {
                    warn!(child = %idle.name, error = %e, "mcp: idle stop failed");
                }
                state.children.forget(&idle.sandbox_id);
            }
        }
    });
}

async fn stop_child(
    state: &Arc<ServerState>,
    caller_id: &str,
    args: &Value,
) -> Result<Value, ToolError> {
    let child = own_child(state, caller_id, args)?;
    cascade_stop_children(state, &child.sandbox_id).await;
    state
        .compute
        .delete_sandbox_by_id(&child.sandbox_id, &child.name)
        .await
        .map_err(|s| terr("INTERNAL", s.message()))?;
    state.children.forget(&child.sandbox_id);
    Ok(json!({"handle": child.name, "stopped": true}))
}

fn list_children(state: &ServerState, caller_id: &str) -> Value {
    let kids: Vec<Value> = state
        .children
        .descendants_deepest_first(caller_id)
        .into_iter()
        .filter(|l| l.parent_id.as_deref() == Some(caller_id))
        .map(
            |l| json!({"handle": l.name, "kind": l.kind, "lifetime": l.lifetime, "depth": l.depth}),
        )
        .collect();
    json!({"children": kids})
}

fn get_limits(state: &ServerState, caller_id: &str) -> Value {
    let Some(l) = state.children.lineage(caller_id) else {
        return json!({"has_grant": false, "can_create_children": false});
    };
    let Some(g) = state.children.grant(&l.grant_id) else {
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
    let inner = state.children.lock();
    let grants: Vec<Value> = inner
        .grants
        .values()
        .map(|g| {
            let holder = inner
                .lineage
                .get(&g.holder_id)
                .map(|l| l.name.clone())
                .unwrap_or_default();
            json!({"grant_id": g.id, "holder": holder, "max_depth": g.max_depth,
                   "max_breadth": g.max_breadth, "max_total": g.max_total, "total": g.total,
                   "program": g.program, "user": g.user.subject})
        })
        .collect();
    json!({"grants": grants})
}

fn grant_update(
    state: &ServerState,
    user: &UserPrincipal,
    args: &Value,
) -> Result<Value, ToolError> {
    let id = str_arg(args, "grant_id")
        .ok_or_else(|| terr("INVALID_ARGUMENT", "grant_id is required"))?;
    let mut inner = state.children.lock();
    if args.get("revoke").and_then(Value::as_bool) == Some(true) {
        inner
            .grants
            .remove(id)
            .ok_or_else(|| terr("NOT_FOUND", "no such grant"))?;
        info!(grant_id = %id, user = %user.identity.subject, "mcp: grant revoked");
        return Ok(json!({"grant_id": id, "revoked": true}));
    }
    let g = inner
        .grants
        .get_mut(id)
        .ok_or_else(|| terr("NOT_FOUND", "no such grant"))?;
    if g.user.subject != user.identity.subject {
        return Err(terr(
            "NOT_ALLOWED",
            "only the user who set the grant can change it",
        ));
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
    Ok(
        json!({"grant_id": id, "max_depth": g.max_depth, "max_breadth": g.max_breadth, "max_total": g.max_total}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_grant_spec() {
        let g = parse_grant_spec(
            "max_depth=1,max_breadth=2,max_total=3,expires=4h,program=/usr/local/bin/node",
        )
        .unwrap();
        assert_eq!(g.max_depth, 1);
        assert_eq!(g.max_breadth, 2);
        assert_eq!(g.max_total, 3);
        assert_eq!(g.expires, Duration::from_secs(4 * 3600));
        assert_eq!(g.program, "/usr/local/bin/node");
    }

    #[test]
    fn parses_child_policy() {
        assert!(!parse_grant_spec("program=/x").unwrap().inherit_child_policy);
        assert!(
            parse_grant_spec("program=/x,child_policy=inherit")
                .unwrap()
                .inherit_child_policy
        );
        assert!(parse_grant_spec("program=/x,child_policy=wide").is_err());
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
    fn tool_sandboxes_never_get_the_door_rule() {
        assert!(gets_door_rule(KIND_AGENT, 1, 2));
        assert!(!gets_door_rule(KIND_AGENT, 2, 2));
        assert!(!gets_door_rule(KIND_TOOL, 1, 2));
        assert!(!gets_door_rule(KIND_TOOL, 1, 9));
    }

    #[test]
    fn descendants_are_deepest_first() {
        let s = ChildrenState::default();
        let mk = |id: &str, parent: Option<&str>, depth| Lineage {
            sandbox_id: id.into(),
            name: id.into(),
            workspace: "w".into(),
            parent_id: parent.map(str::to_string),
            depth,
            grant_id: "g".into(),
            kind: "agent".into(),
            lifetime: "until_stopped".into(),
            children_created: 0,
            idle_timeout_ms: None,
            last_used_ms: 0,
        };
        {
            let mut i = s.lock();
            i.lineage.insert("lead".into(), mk("lead", None, 0));
            i.lineage.insert("a".into(), mk("a", Some("lead"), 1));
            i.lineage.insert("a1".into(), mk("a1", Some("a"), 2));
        }
        let d: Vec<String> = s
            .descendants_deepest_first("lead")
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(d, vec!["a1".to_string(), "a".to_string()]);
    }
}
