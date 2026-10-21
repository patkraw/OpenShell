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
//! - every child policy is proved to fit inside its **parent's policy**;
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
/// Annotation on a child sandbox naming its parent's sandbox id. Later writes
/// to the child's policy are proved against that parent's policy.
pub const PARENT_ANNOTATION: &str = "openshell.io/mcp-parent";
/// Header in which the supervisor door names the calling program (the
/// kernel-resolved executable path). The door overwrites any value the
/// workload sends.
pub const CALLER_PROGRAM_HEADER: &str = "x-openshell-caller-program";

pub const KIND_AGENT: &str = "agent";
pub const KIND_TOOL: &str = "tool";
/// Default idle timeout for tool sandboxes.
const DEFAULT_TOOL_IDLE: Duration = Duration::from_secs(600);
const REAPER_INTERVAL: Duration = Duration::from_secs(5);
const MAX_EXEC_TIMEOUT: Duration = Duration::from_secs(300);

/// Whether a new child may create children of its own under the grant as it
/// is now. Tool sandboxes run commands for their parent and never create
/// children. The door is open in every sandbox; the gateway decides per call.
pub fn may_create_children(kind: &str, depth: u32, max_depth: u32) -> bool {
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
    /// Children get their parent's policy unchanged; agents may not write child policies.
    pub inherit_child_policy: bool,
    /// Program allowed to use the door, e.g. `/usr/local/bin/node`.
    pub program: String,
    pub expires_at_ms: i64,
    pub total: u32,
    /// Workspace of the holder; Workspace Admins of it may manage the grant.
    pub workspace: String,
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
    /// Children this sandbox has created under the grant, including ones that
    /// have since stopped (breadth counts creations, not running children).
    pub children_created: u32,
    /// The user the tree acts for. Kept on the lineage so that a parent can
    /// still wait for, read from, run commands in and stop its existing
    /// children after the grant is revoked or expires.
    pub user: Identity,
    /// Tool sandboxes: stopped after this long without an `exec` / `get_file`.
    pub idle_timeout_ms: Option<i64>,
    pub last_used_ms: i64,
}

/// A retried `create_sandbox` by `request_id`, scoped to the parent.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RequestState {
    /// The first request is still launching the child. Holds the
    /// fingerprint of its arguments.
    InFlight(String),
    /// The first request created this child: (sandbox name, fingerprint).
    Done(String, String),
}

/// A `request_id` and the fingerprint of the arguments it was sent with.
type RequestRef<'a> = (&'a str, &'a str);

/// Fingerprint of a create request's arguments, without `request_id`, so a
/// retry with the same arguments is recognised and one with different
/// arguments is refused instead of silently returning the first child.
fn request_fingerprint(args: &Value) -> String {
    use sha2::{Digest, Sha256};
    fn canonical(v: &Value) -> Value {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                let mut out = serde_json::Map::new();
                for k in keys {
                    out.insert(k.clone(), canonical(&map[k]));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
            other => other.clone(),
        }
    }
    let mut args = args.clone();
    if let Some(obj) = args.as_object_mut() {
        obj.remove("request_id");
    }
    let text = canonical(&args).to_string();
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

#[derive(Debug, Default)]
struct Inner {
    grants: HashMap<String, Grant>,
    lineage: HashMap<String, Lineage>,
    /// (parent sandbox id, request_id) -> state of the first request.
    requests: HashMap<(String, String), RequestState>,
    /// Child sandbox id -> the final result it submitted for its parent. In
    /// memory, like lineage: lost on a gateway restart.
    results: HashMap<String, String>,
}

/// The outcome of reserving a child slot under the grant.
#[derive(Debug)]
enum Reservation {
    /// A slot is reserved; counters are already charged.
    Reserved { depth: u32, grant: Grant },
    /// The same parent already created a child for this `request_id`.
    Repeated(String),
}

/// Why a sandbox in a tree may not create a child of `kind` now.
fn check_limits(
    lineage: &Lineage,
    grant: Option<&Grant>,
    kind: &str,
    now: i64,
) -> Result<Grant, ToolError> {
    if lineage.kind == KIND_TOOL {
        return Err(terr("NOT_ALLOWED", "tool sandboxes cannot create children"));
    }
    let grant = grant.ok_or_else(|| terr("NOT_ALLOWED", "grant revoked"))?;
    if now > grant.expires_at_ms {
        return Err(terr("NOT_ALLOWED", "grant expired"));
    }
    if kind == KIND_AGENT && lineage.depth >= grant.max_depth {
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
    Ok(grant.clone())
}

impl Inner {
    /// The outcome of an earlier request with the same parent and
    /// `request_id`: its child's handle, `IN_PROGRESS`, or a refusal if the
    /// arguments differ.
    fn earlier_request(
        &self,
        parent_id: &str,
        (rid, fingerprint): RequestRef<'_>,
    ) -> Result<Option<String>, ToolError> {
        let reused = || {
            terr(
                "INVALID_ARGUMENT",
                format!("request_id '{rid}' was already used with different arguments"),
            )
        };
        match self.requests.get(&(parent_id.to_string(), rid.to_string())) {
            Some(RequestState::Done(handle, fp)) if fp == fingerprint => Ok(Some(handle.clone())),
            Some(RequestState::InFlight(fp)) if fp == fingerprint => Err(terr(
                "IN_PROGRESS",
                format!("request_id '{rid}' is still being created; retry shortly"),
            )),
            Some(_) => Err(reused()),
            None => Ok(None),
        }
    }

    /// Check every limit and charge the counters in one step, under the lock,
    /// so concurrent creates cannot both take the last slot. A `request_id`
    /// is marked in flight here, before the child is launched, so a duplicate
    /// either gets the first child's handle or `IN_PROGRESS`, never a second
    /// child.
    fn reserve(
        &mut self,
        parent_id: &str,
        kind: &str,
        request: Option<RequestRef<'_>>,
        now: i64,
    ) -> Result<Reservation, ToolError> {
        if let Some(req) = request
            && let Some(handle) = self.earlier_request(parent_id, req)?
        {
            return Ok(Reservation::Repeated(handle));
        }
        let lineage = self
            .lineage
            .get(parent_id)
            .cloned()
            .ok_or_else(|| terr("NOT_ALLOWED", "this sandbox has no grant"))?;
        let grant = check_limits(&lineage, self.grants.get(&lineage.grant_id), kind, now)?;
        if let Some(g) = self.grants.get_mut(&grant.id) {
            g.total += 1;
        }
        if let Some(l) = self.lineage.get_mut(parent_id) {
            l.children_created += 1;
        }
        if let Some((rid, fingerprint)) = request {
            self.requests.insert(
                (parent_id.to_string(), rid.to_string()),
                RequestState::InFlight(fingerprint.to_string()),
            );
        }
        Ok(Reservation::Reserved {
            depth: lineage.depth + 1,
            grant,
        })
    }

    /// Undo a reservation whose child was not created.
    fn release(&mut self, parent_id: &str, grant_id: &str, request_id: Option<&str>) {
        if let Some(g) = self.grants.get_mut(grant_id) {
            g.total = g.total.saturating_sub(1);
        }
        if let Some(l) = self.lineage.get_mut(parent_id) {
            l.children_created = l.children_created.saturating_sub(1);
        }
        if let Some(rid) = request_id {
            self.requests
                .remove(&(parent_id.to_string(), rid.to_string()));
        }
    }

    /// Record the child a reserved `request_id` created.
    fn complete(&mut self, parent_id: &str, request: Option<RequestRef<'_>>, handle: &str) {
        if let Some((rid, fingerprint)) = request {
            self.requests.insert(
                (parent_id.to_string(), rid.to_string()),
                RequestState::Done(handle.to_string(), fingerprint.to_string()),
            );
        }
    }

    /// The grant as returned to users.
    fn grant_json(&self, g: &Grant) -> Value {
        let holder = self
            .lineage
            .get(&g.holder_id)
            .map(|l| l.name.clone())
            .unwrap_or_default();
        json!({"grant_id": g.id, "holder": holder, "max_depth": g.max_depth,
               "max_breadth": g.max_breadth, "max_total": g.max_total, "total": g.total,
               "program": g.program, "user": g.user.subject, "workspace": g.workspace})
    }
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

    /// Keep a child's result. One per child: the same text again is a no-op
    /// (`Ok(true)`), different text is refused.
    fn submit_result(&self, sandbox_id: &str, result: &str) -> Result<bool, ToolError> {
        let mut inner = self.lock();
        match inner.results.get(sandbox_id) {
            Some(prev) if prev == result => Ok(true),
            Some(_) => Err(terr(
                "ALREADY_SUBMITTED",
                "a different result was already submitted; a child submits one final result",
            )),
            None => {
                inner
                    .results
                    .insert(sandbox_id.to_string(), result.to_string());
                Ok(false)
            }
        }
    }

    fn result_of(&self, sandbox_id: &str) -> Option<String> {
        self.lock().results.get(sandbox_id).cloned()
    }

    pub fn forget(&self, sandbox_id: &str) {
        let mut inner = self.lock();
        inner.lineage.remove(sandbox_id);
        inner.results.remove(sandbox_id);
        inner.grants.retain(|_, g| g.holder_id != sandbox_id);
        inner.requests.retain(|(parent, _), _| parent != sandbox_id);
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
    /// `child_policy=inherit`: every child gets its parent's policy unchanged.
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

/// The door forwards every sandbox's MCP calls. A sandbox whose tree has a
/// grant may only call through the grant's program.
fn check_caller_program(
    state: &ServerState,
    sandbox_id: &str,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    let Some(grant) = state
        .children
        .lineage(sandbox_id)
        .and_then(|l| state.children.grant(&l.grant_id))
    else {
        return Ok(());
    };
    let program = headers
        .get(CALLER_PROGRAM_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if program == grant.program {
        Ok(())
    } else {
        Err(ApiError::new(
            "PROGRAM_NOT_ALLOWED",
            format!(
                "calls from '{program}' are not allowed; this tree's grant allows '{}'",
                grant.program
            ),
        ))
    }
}

// ---------------------------------------------------------------------------
// Hooks called from CreateSandbox / DeleteSandbox
// ---------------------------------------------------------------------------

/// Before a user's create: if it carries a grant, check who sets it and make
/// the policy explicit. Returns the parsed grant spec to register after the
/// sandbox exists.
pub fn prepare_granted_create(
    principal: &Principal,
    annotations: &HashMap<String, String>,
    spec: &mut SandboxSpec,
) -> Result<Option<GrantSpec>, tonic::Status> {
    let Some(raw) = annotations.get(GRANT_ANNOTATION) else {
        return Ok(None);
    };
    if !matches!(principal, Principal::User(_)) {
        return Err(tonic::Status::permission_denied(
            "only a user can set a grant",
        ));
    }
    let grant = parse_grant_spec(raw).map_err(tonic::Status::invalid_argument)?;
    // A tree always has an explicit policy: without one, the restrictive
    // default (what a supervisor would fall back to) bounds its children.
    spec.policy
        .get_or_insert_with(openshell_policy::restrictive_default_policy);
    Ok(Some(grant))
}

/// A new grant held by `holder`, with `total` sandboxes already in its tree.
fn new_grant(user: &UserPrincipal, holder: &Sandbox, spec: GrantSpec, total: u32) -> Grant {
    let expires_ms = i64::try_from(spec.expires.as_millis()).unwrap_or(i64::MAX);
    Grant {
        id: format!("gr-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
        user: user.identity.clone(),
        holder_id: holder.object_id().to_string(),
        max_depth: spec.max_depth,
        max_breadth: spec.max_breadth,
        max_total: spec.max_total,
        program: spec.program,
        inherit_child_policy: spec.inherit_child_policy,
        expires_at_ms: now_ms().saturating_add(expires_ms),
        total,
        workspace: holder.object_workspace().to_string(),
    }
}

/// Whether `principal` may see and manage `grant`: the user who set it, or a
/// Workspace Admin of the holder's workspace.
async fn may_manage_grant(
    state: &ServerState,
    principal: &Principal,
    user: &UserPrincipal,
    grant: &Grant,
) -> bool {
    if grant.user.subject == user.identity.subject {
        return true;
    }
    crate::auth::workspace_authz::authorize_workspace(
        &state.store,
        &state.admin_role,
        principal,
        &grant.workspace,
        crate::auth::workspace_authz::MinWorkspaceRole::Admin,
    )
    .await
    .is_ok()
}

pub fn register_granted_sandbox(
    state: &ServerState,
    principal: &Principal,
    sandbox: &Sandbox,
    grant: GrantSpec,
) -> Option<String> {
    let Principal::User(user) = principal else {
        return None;
    };
    let new = new_grant(user, sandbox, grant, 1);
    let grant_id = new.id.clone();
    let mut inner = state.children.lock();
    inner.grants.insert(grant_id.clone(), new);
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
            user: user.identity.clone(),
            idle_timeout_ms: None,
            last_used_ms: now_ms(),
        },
    );
    info!(grant_id = %grant_id, holder = %sandbox.object_name(), "mcp: grant registered");
    Some(grant_id)
}

/// A user attaches a grant to a running top-level sandbox. The sandbox becomes
/// the holder; its current policy bounds its children. If the sandbox already
/// holds a grant that was revoked or has expired, the new grant replaces it
/// for the whole tree: running children stay, and count toward the new total.
async fn grant_create(
    state: &Arc<ServerState>,
    user: &UserPrincipal,
    workspace: &str,
    sandbox_name: &str,
    spec: &str,
) -> Result<Value, ToolError> {
    let sandbox = state
        .store
        .get_message_by_name::<Sandbox>(workspace, sandbox_name)
        .await
        .map_err(|e| terr("INTERNAL", e.to_string()))?
        .ok_or_else(|| terr("NOT_FOUND", format!("sandbox '{sandbox_name}' not found")))?;
    let grant = parse_grant_spec(spec).map_err(|e| terr("INVALID_ARGUMENT", e))?;
    let principal = Principal::User(user.clone());
    let Some(holder) = state.children.lineage(sandbox.object_id()) else {
        let grant_id = register_granted_sandbox(state, &principal, &sandbox, grant)
            .ok_or_else(|| terr("INTERNAL", "grant not registered"))?;
        info!(grant_id = %grant_id, holder = %sandbox_name, user = %user.identity.subject, "mcp: grant attached to a running sandbox");
        return grant_by_id(state, &grant_id);
    };
    if holder.parent_id.is_some() {
        return Err(terr(
            "NOT_ALLOWED",
            format!("'{sandbox_name}' is a child in a tree; its tree's grant already covers it"),
        ));
    }
    // The tree's current grant: active means no replacement; an expired one
    // may only be replaced by someone who may manage it.
    if let Some(old) = state.children.grant(&holder.grant_id) {
        if now_ms() <= old.expires_at_ms {
            return Err(terr(
                "ALREADY_EXISTS",
                format!(
                    "'{sandbox_name}' already holds grant {}; revoke it first to replace it",
                    old.id
                ),
            ));
        }
        if !may_manage_grant(state, &principal, user, &old).await {
            return Err(terr(
                "NOT_ALLOWED",
                "only the user who set the expired grant, or a Workspace Admin, can replace it",
            ));
        }
    }
    let grant_id = {
        let mut inner = state.children.lock();
        let old_id = holder.grant_id.clone();
        let total = u32::try_from(
            inner
                .lineage
                .values()
                .filter(|l| l.grant_id == old_id)
                .count(),
        )
        .unwrap_or(u32::MAX);
        let new = new_grant(user, &sandbox, grant, total);
        let new_id = new.id.clone();
        inner.grants.remove(&old_id);
        inner.grants.insert(new_id.clone(), new);
        for l in inner.lineage.values_mut().filter(|l| l.grant_id == old_id) {
            l.grant_id = new_id.clone();
        }
        new_id
    };
    info!(grant_id = %grant_id, holder = %sandbox_name, user = %user.identity.subject, "mcp: grant replaced for an existing tree");
    grant_by_id(state, &grant_id)
}

fn grant_by_id(state: &ServerState, grant_id: &str) -> Result<Value, ToolError> {
    let inner = state.children.lock();
    let g = inner
        .grants
        .get(grant_id)
        .ok_or_else(|| terr("INTERNAL", "grant not found after registration"))?;
    Ok(inner.grant_json(g))
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
    ToolError {
        code,
        message: message.into(),
    }
}

/// Whether `sandbox_id` may create a child of `kind` now. Tool sandboxes
/// ignore the depth limit, so a sandbox at the bottom of the tree can still
/// run commands in its own tool sandboxes; they count toward breadth and total.
/// Used for early refusals and `get_limits`; `Inner::reserve` repeats the
/// checks under the lock when the slot is taken.
fn can_create(
    state: &ServerState,
    sandbox_id: &str,
    kind: &str,
) -> Result<(Lineage, Grant), ToolError> {
    let inner = state.children.lock();
    let lineage = inner
        .lineage
        .get(sandbox_id)
        .cloned()
        .ok_or_else(|| terr("NOT_ALLOWED", "this sandbox has no grant"))?;
    let grant = check_limits(
        &lineage,
        inner.grants.get(&lineage.grant_id),
        kind,
        now_ms(),
    )?;
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
        let principal = authenticate(&self.state, headers)
            .await
            .map_err(|m| ApiError::new("UNAUTHENTICATED", m))?;
        if let Principal::Sandbox(s) = &principal {
            check_caller_program(&self.state, &s.sandbox_id, headers)?;
        }
        Ok(principal)
    }

    /// Sandboxes always see the child tools: a grant can be widened at any
    /// time, so a call is refused (`LIMIT_REACHED`, `NOT_ALLOWED`) rather than
    /// the tool hidden. `get_limits` says what is allowed right now.
    async fn access(&self, caller: &Principal) -> Access {
        match caller {
            Principal::Sandbox(_) => Access {
                create_sandbox: true,
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
            (Principal::Sandbox(s), Operation::SubmitResult(a)) => {
                submit_result(state, &s.sandbox_id, &a.result)
            }
            (Principal::User(u), Operation::GrantList) => Ok(grant_list(state, caller, u).await),
            (Principal::User(u), Operation::GrantCreate(a)) => {
                grant_create(state, u, "default", &a.sandbox, &a.grant).await
            }
            (Principal::User(u), Operation::GrantUpdate(a)) => {
                grant_update(state, caller, u, &val(&a)).await
            }
            (_, Operation::GrantList | Operation::GrantCreate(_) | Operation::GrantUpdate(_)) => {
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

/// The part of a parent's policy the prover can model. Endpoints with a
/// protocol it does not model (MCP, GraphQL, SQL, ...) are dropped, and rules
/// left without endpoints are removed. Dropping allow rules only shrinks the
/// boundary, so a child that fits the result also fits the full policy; without
/// this, one MCP rule in the parent would make every proof `Unsupported`.
fn modelable_boundary(policy: &SandboxPolicy) -> SandboxPolicy {
    let mut boundary = policy.clone();
    boundary.network_policies.retain(|_, rule| {
        rule.endpoints
            .retain(|e| matches!(e.protocol.as_str(), "" | "tcp" | "rest"));
        !rule.endpoints.is_empty()
    });
    boundary
}

/// Next step for an agent whose child policy was refused, in the style of the
/// supervisor's `policy.local` guidance for blocked requests.
const EXCEEDS_HINT: &str = "A child cannot get more than you have. Either narrow the \
    child's policy and retry, or, if the task needs this access and policy proposals are \
    enabled in this sandbox, first ask for it for yourself: read /etc/openshell/skills/policy_advisor.md, submit the narrowest rule to \
    http://policy.local/v1/proposals, wait for approval and `policy_reloaded: true`, then \
    retry create_sandbox.";

/// Prove `candidate` fits inside `ceiling`. Only `Within` passes.
async fn prove(ceiling: &SandboxPolicy, candidate: &SandboxPolicy) -> Result<(), ToolError> {
    let ser = |p: &SandboxPolicy| {
        openshell_policy::serialize_sandbox_policy(p)
            .map_err(|e| terr("INVALID_POLICY", e.to_string()))
            .and_then(|y| {
                containment::parse_policy_str(&y).map_err(|e| terr("INVALID_POLICY", e.to_string()))
            })
    };
    let boundary = ser(&modelable_boundary(ceiling))?;
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
                "child policy exceeds the parent's policy; counterexample: {:?}",
                ev.counterexample()
            ),
        )),
        CheckResult::Unsupported(ev) => Err(terr("UNSUPPORTED_RULES", ev.reason().to_string())),
        CheckResult::Inconclusive(ev) => Err(terr("PROVER_INCONCLUSIVE", ev.reason().to_string())),
    }
}

/// Add the next-step hint to a create refused for exceeding the parent.
fn with_create_hint(err: ToolError) -> ToolError {
    if err.code == "EXCEEDS_CEILING" {
        ToolError {
            code: err.code,
            message: format!("{}. {EXCEEDS_HINT}", err.message),
        }
    } else {
        err
    }
}

/// Later writes to a child's policy (user `UpdateConfig`, supervisor sync,
/// approved or auto-approved proposals) must stay within its parent's current
/// policy, the same rule as at creation. Sandboxes that are not children, and
/// writes that leave the policy unchanged, pass.
pub(crate) async fn check_child_policy_write(
    store: &crate::persistence::Store,
    sandbox_id: &str,
    candidate: &SandboxPolicy,
) -> Result<(), tonic::Status> {
    let internal = |e: String| tonic::Status::internal(e);
    let Some(child) = store
        .get_message::<Sandbox>(sandbox_id)
        .await
        .map_err(|e| internal(e.to_string()))?
    else {
        return Ok(());
    };
    let Some(parent_id) = child
        .metadata
        .as_ref()
        .and_then(|m| m.annotations.get(PARENT_ANNOTATION))
        .cloned()
    else {
        return Ok(());
    };
    let current = crate::grpc::policy::current_base_policy_for_sandbox(store, &child).await?;
    if current == *candidate {
        return Ok(());
    }
    let parent = store
        .get_message::<Sandbox>(&parent_id)
        .await
        .map_err(|e| internal(e.to_string()))?
        .ok_or_else(|| {
            tonic::Status::failed_precondition("this sandbox's parent no longer exists")
        })?;
    let ceiling = crate::grpc::policy::current_base_policy_for_sandbox(store, &parent).await?;
    prove(&ceiling, candidate).await.map_err(|e| {
        warn!(
            sandbox_id,
            code = e.code,
            "policy write refused: exceeds parent's policy"
        );
        tonic::Status::failed_precondition(format!(
            "{}: a child sandbox's policy must stay within its parent's policy ({}); {}",
            e.code,
            parent.object_name(),
            e.message
        ))
    })
}

async fn create_child(
    state: &Arc<ServerState>,
    caller_id: &str,
    args: &Value,
) -> Result<Value, ToolError> {
    // 1. Retry: the same parent and request_id return the first result,
    //    counted once. A duplicate that arrives while the first is still
    //    launching gets IN_PROGRESS (checked again when the slot is reserved).
    //    The same request_id with different arguments is refused.
    let request_id = str_arg(args, "request_id").map(str::to_string);
    let fingerprint = request_fingerprint(args);
    let request = request_id.as_deref().map(|rid| (rid, fingerprint.as_str()));
    if let Some(req) = request
        && let Some(name) = state.children.lock().earlier_request(caller_id, req)?
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
    let kind = str_arg(args, "kind").unwrap_or(KIND_AGENT).to_string();
    if kind != KIND_AGENT && kind != KIND_TOOL {
        return Err(terr(
            "INVALID_ARGUMENT",
            "kind must be \"agent\" or \"tool\"",
        ));
    }
    // Early refusal before the proof; the limits are checked again, under the
    // lock, when the slot is reserved in step 7.
    let (parent_lineage, grant) = can_create(state, caller_id, &kind)?;
    let parent = state
        .store
        .get_message::<Sandbox>(caller_id)
        .await
        .map_err(|e| terr("INTERNAL", e.to_string()))?
        .ok_or_else(|| terr("NOT_FOUND", "caller sandbox not found"))?;
    let parent_spec = parent.spec.clone().unwrap_or_default();

    // 4. The child's ceiling is its parent's current policy, so each level can
    //    only narrow. The agent may narrow every section the prover checks:
    //    filesystem, process and Landlock sections it omits are the parent's;
    //    network rules it omits are none (least privilege by default).
    let ceiling = crate::grpc::policy::current_base_policy_for_sandbox(&state.store, &parent)
        .await
        .map_err(|s| terr("INTERNAL", s.message()))?;
    let mut candidate = ceiling.clone();
    candidate.network_policies.clear();
    let wants_policy = args.get("policy").is_some_and(|p| !p.is_null());
    if grant.inherit_child_policy {
        if wants_policy {
            return Err(terr(
                "INVALID_ARGUMENT",
                "this tree's grant gives every child its parent's policy; omit policy",
            ));
        }
        candidate.network_policies = ceiling.network_policies.clone();
    }
    if let Some(p) = args.get("policy").filter(|p| !p.is_null()) {
        let mut p = p.clone();
        if let Some(obj) = p.as_object_mut() {
            obj.entry("version").or_insert(json!(1));
        }
        let parsed = openshell_policy::parse_sandbox_policy(&p.to_string())
            .map_err(|e| terr("INVALID_POLICY", e.to_string()))?;
        let has = |key: &str| p.get(key).is_some_and(|v| !v.is_null());
        if has("filesystem_policy") {
            candidate.filesystem = parsed.filesystem;
        }
        if has("process") {
            candidate.process = parsed.process;
        }
        if has("landlock") {
            candidate.landlock = parsed.landlock;
        }
        if has("network_middlewares") {
            return Err(terr(
                "INVALID_POLICY",
                "network middlewares are not proved and cannot be set for a child",
            ));
        }
        candidate.network_policies = parsed.network_policies;
    }
    // 5. Prove against the parent's policy. An inherited policy is the
    //    parent's own, so it fits by construction, including rules the prover
    //    cannot model.
    if !grant.inherit_child_policy {
        prove(&ceiling, &candidate)
            .await
            .map_err(with_create_hint)?;
    }

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

    let proved_grant_id = grant.id.clone();
    let proved_inherit = grant.inherit_child_policy;

    // 7. Reserve the slot: every limit is checked again and charged in one
    //    step under the lock, and the request_id is marked in flight.
    let reservation = state
        .children
        .lock()
        .reserve(caller_id, &kind, request, now_ms())?;
    let (depth, grant) = match reservation {
        Reservation::Repeated(name) => {
            return Ok(json!({"handle": name, "repeated_request": true}));
        }
        Reservation::Reserved { depth, grant } => (depth, grant),
    };
    let rollback = |state: &ServerState| {
        state
            .children
            .lock()
            .release(caller_id, &grant.id, request_id.as_deref());
    };

    // 8. Stale admission: the proof used the grant and the parent's policy as
    //    they were before the reservation. If either changed meanwhile (the
    //    grant was replaced, or the parent's policy was narrowed), give the
    //    slot back and let the caller retry against the new state.
    let stale = if grant.id != proved_grant_id || grant.inherit_child_policy != proved_inherit {
        Some("the tree's grant changed")
    } else {
        match crate::grpc::policy::current_base_policy_for_sandbox(&state.store, &parent).await {
            Ok(now) if now == ceiling => None,
            Ok(_) => Some("the parent's policy changed"),
            Err(s) => {
                rollback(state);
                return Err(terr("INTERNAL", s.message()));
            }
        }
    };
    if let Some(why) = stale {
        rollback(state);
        return Err(terr(
            "STALE_ADMISSION",
            format!("{why} while this child was being admitted; retry create_sandbox"),
        ));
    }

    let policy = candidate;

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
        annotations: HashMap::from([(
            PARENT_ANNOTATION.to_string(),
            parent.object_id().to_string(),
        )]),
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
                user: grant.user.clone(),
                idle_timeout_ms,
                last_used_ms: now_ms(),
            },
        );
        inner.complete(caller_id, request, created.object_name());
    }
    if kind == KIND_TOOL {
        start_idle_reaper(state);
    }
    info!(parent = %parent.object_name(), child = %created.object_name(), depth, %kind, "mcp: child created");
    Ok(json!({
        "handle": created.object_name(),
        "depth": depth,
        "can_create_children": may_create_children(&kind, depth, grant.max_depth),
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
            && args.get("until_exit").is_none()
            && args.get("for_result").and_then(Value::as_bool) != Some(true);
        // A submitted result ends the wait even while the child keeps running.
        let result = state.children.result_of(&child.sandbox_id);
        if exited || ready || result.is_some() || tokio::time::Instant::now() >= deadline {
            return Ok(json!({
                "handle": child.name,
                "phase": phase.as_str_name(),
                "exit_code": status.exit_code,
                "result": result,
                "timed_out": !exited && !ready && result.is_none()
            }));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

const MAX_RESULT_BYTES: usize = 64 * 1024;

/// A child hands its final result to its parent; the parent's `wait` returns
/// it. The caller is the authenticated sandbox, so a child can only submit its
/// own result.
fn submit_result(state: &ServerState, caller_id: &str, result: &str) -> Result<Value, ToolError> {
    let me = state
        .children
        .lineage(caller_id)
        .filter(|l| l.parent_id.is_some())
        .ok_or_else(|| {
            terr(
                "NOT_A_CHILD",
                "only a child sandbox has a parent to submit to",
            )
        })?;
    if result.len() > MAX_RESULT_BYTES {
        return Err(terr(
            "TOO_LARGE",
            format!(
                "result is {} bytes; the limit is {MAX_RESULT_BYTES}. Leave larger output in a file \
                 for the parent to read with get_file",
                result.len()
            ),
        ));
    }
    let repeated = state.children.submit_result(caller_id, result)?;
    Ok(json!({"handle": me.name, "saved": true, "repeated": repeated}))
}

async fn child_output(
    state: &Arc<ServerState>,
    caller_id: &str,
    args: &Value,
) -> Result<Value, ToolError> {
    let child = own_child(state, caller_id, args)?;
    let mut req = tonic::Request::new(GetSandboxLogsRequest {
        workspace_scope: Some(workspace_selector(child.workspace.clone())),
        sandbox: child.name.clone(),
        lines: 200,
        ..Default::default()
    });
    req.extensions_mut().insert(Principal::User(UserPrincipal {
        identity: child.user.clone(),
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
    let mut req = tonic::Request::new(ExecSandboxRequest {
        workspace_scope: Some(workspace_selector(child.workspace.clone())),
        sandbox: child.name.clone(),
        command,
        no_login_shell: true,
        execution_timeout: prost_types::Duration::try_from(timeout).ok(),
        ..Default::default()
    });
    req.extensions_mut().insert(Principal::User(UserPrincipal {
        identity: child.user.clone(),
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
        "can_create_children": can_create(state, caller_id, KIND_AGENT).is_ok(),
        "can_create_tools": can_create(state, caller_id, KIND_TOOL).is_ok()
    })
}

/// Grants the caller may see: those it set, and those in workspaces it
/// administers.
async fn grant_list(state: &ServerState, principal: &Principal, user: &UserPrincipal) -> Value {
    let all: Vec<Grant> = state.children.lock().grants.values().cloned().collect();
    let mut visible = Vec::new();
    for g in all {
        if may_manage_grant(state, principal, user, &g).await {
            visible.push(g.id);
        }
    }
    let inner = state.children.lock();
    let grants: Vec<Value> = visible
        .iter()
        .filter_map(|id| inner.grants.get(id))
        .map(|g| inner.grant_json(g))
        .collect();
    json!({"grants": grants})
}

/// Change or revoke a grant. Ownership is checked before anything changes;
/// a grant the caller may not see is reported as not found.
async fn grant_update(
    state: &ServerState,
    principal: &Principal,
    user: &UserPrincipal,
    args: &Value,
) -> Result<Value, ToolError> {
    let id = str_arg(args, "grant_id")
        .ok_or_else(|| terr("INVALID_ARGUMENT", "grant_id is required"))?;
    let grant = state
        .children
        .grant(id)
        .ok_or_else(|| terr("NOT_FOUND", "no such grant"))?;
    if !may_manage_grant(state, principal, user, &grant).await {
        return Err(terr(
            "NOT_ALLOWED",
            "only the user who set the grant, or a Workspace Admin, can change it",
        ));
    }
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
    let set = |v: &mut u32, key: &str| {
        if let Some(n) = args.get(key).and_then(Value::as_u64) {
            *v = u32::try_from(n).unwrap_or(u32::MAX);
        }
    };
    set(&mut g.max_depth, "max_depth");
    set(&mut g.max_breadth, "max_breadth");
    set(&mut g.max_total, "max_total");
    info!(grant_id = %id, user = %user.identity.subject, ?args, "mcp: grant updated (applies from the next call)");
    Ok(
        json!({"grant_id": id, "max_depth": g.max_depth, "max_breadth": g.max_breadth, "max_total": g.max_total}),
    )
}

// ---------------------------------------------------------------------------
// gRPC surface: the same rules as MCP, through `GatewaySandboxApi::call`
// ---------------------------------------------------------------------------

/// gRPC handlers for the child-sandbox and grant RPCs. Each one builds the
/// same `Operation` the MCP surface builds, so both surfaces enforce the same
/// rules in the same code.
pub(crate) mod grpc_surface {
    use super::{ApiError, GatewaySandboxApi, ServerState, list_children};
    use crate::auth::principal::Principal;
    use crate::auth::workspace_authz::{
        MinWorkspaceRole, authorize_workspace, selected_workspace_name,
    };
    use crate::pagination::Pagination;
    use openshell_core::proto as pb;
    use openshell_core::{ObjectId, proto::Sandbox};
    use openshell_gateway_mcp::{
        CreateSandboxArgs, ExecArgs, FileArgs, GrantUpdateArgs, HandleArgs, Operation, SandboxApi,
        WaitArgs,
    };
    use serde_json::Value;
    use std::sync::Arc;
    use tonic::{Request, Response, Status};

    fn status_of(e: ApiError) -> Status {
        let msg = format!("{}: {}", e.code, e.message);
        match e.code.as_str() {
            "NOT_ALLOWED" | "NOT_YOUR_CHILD" | "WRONG_KIND" | "GLOBAL_POLICY_ACTIVE" => {
                Status::permission_denied(msg)
            }
            "LIMIT_REACHED" => Status::resource_exhausted(msg),
            "EXCEEDS_CEILING" | "UNSUPPORTED_RULES" | "INVALID_ARGUMENT" | "INVALID_POLICY" => {
                Status::invalid_argument(msg)
            }
            "NOT_FOUND" => Status::not_found(msg),
            "ALREADY_EXISTS" => Status::already_exists(msg),
            "UNAUTHENTICATED" => Status::unauthenticated(msg),
            "PROVER_INCONCLUSIVE" => Status::failed_precondition(msg),
            "IN_PROGRESS" | "STALE_ADMISSION" => Status::aborted(msg),
            _ => Status::internal(msg),
        }
    }

    async fn run<T>(
        state: &Arc<ServerState>,
        req: &Request<T>,
        op: Operation,
    ) -> Result<Value, Status> {
        let principal = crate::grpc::extract_principal(req)?;
        GatewaySandboxApi::new(state.clone())
            .call(&principal, op)
            .await
            .map_err(status_of)
    }

    fn s(v: &Value, k: &str) -> String {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
    fn n(v: &Value, k: &str) -> u32 {
        v.get(k)
            .and_then(Value::as_u64)
            .and_then(|x| u32::try_from(x).ok())
            .unwrap_or_default()
    }
    fn b(v: &Value, k: &str) -> bool {
        v.get(k).and_then(Value::as_bool).unwrap_or_default()
    }
    fn opt(x: String) -> Option<String> {
        (!x.trim().is_empty()).then_some(x)
    }
    /// An optional request duration to whole seconds for the shared rules.
    /// Unset or zero means the operation's default.
    fn timeout(value: Option<&prost_types::Duration>) -> Result<Option<u64>, Status> {
        let Some(value) = value else {
            return Ok(None);
        };
        let duration = openshell_core::time::duration_to_std(value)
            .map_err(|e| Status::invalid_argument(format!("INVALID_ARGUMENT: {e}")))?;
        let secs = duration.as_secs();
        Ok((secs > 0).then_some(secs))
    }

    /// One page of `items`, ordered by `key`, after the token's key.
    fn page<T>(
        mut items: Vec<T>,
        key: impl Fn(&T) -> &str,
        pagination: &Pagination,
    ) -> Result<(Vec<T>, String), Status> {
        items.sort_by(|a, b| key(a).cmp(key(b)));
        if let Some(after) = pagination.profile_cursor()? {
            items.retain(|item| key(item) > after);
        }
        let size = usize::try_from(pagination.page_size()).unwrap_or(usize::MAX);
        if items.len() <= size {
            return Ok((items, String::new()));
        }
        items.truncate(size);
        let next = pagination.next_profile_token(items.last().map(&key));
        Ok((items, next))
    }

    #[cfg(test)]
    mod tests {
        use super::{Pagination, page};

        #[test]
        fn pages_follow_the_key_order() {
            let items = vec!["c", "a", "b"];
            let first = Pagination::new(2, "", "ListSandboxGrants", &[]).unwrap();
            let (got, next) = page(items.clone(), |s| s, &first).unwrap();
            assert_eq!(got, ["a", "b"]);
            assert!(!next.is_empty());
            let second = Pagination::new(2, &next, "ListSandboxGrants", &[]).unwrap();
            let (got, next) = page(items, |s| s, &second).unwrap();
            assert_eq!(got, ["c"]);
            assert!(next.is_empty());
        }
    }

    /// Policy text (JSON or YAML) to the JSON value the rules take.
    fn policy_value(text: &str) -> Result<Option<Value>, Status> {
        if text.trim().is_empty() {
            return Ok(None);
        }
        let parsed = openshell_policy::parse_sandbox_policy(text)
            .map_err(|e| Status::invalid_argument(format!("INVALID_POLICY: {e}")))?;
        let json = openshell_policy::serialize_sandbox_policy_json(&parsed)
            .map_err(|e| Status::invalid_argument(format!("INVALID_POLICY: {e}")))?;
        serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| Status::internal(e.to_string()))
    }

    pub(crate) async fn create_child(
        state: &Arc<ServerState>,
        req: Request<pb::CreateChildSandboxRequest>,
    ) -> Result<Response<pb::CreateChildSandboxResponse>, Status> {
        let r = req.get_ref().clone();
        let args = CreateSandboxArgs {
            command: r.command,
            kind: opt(r.kind),
            lifetime: opt(r.lifetime),
            name: opt(r.name),
            task: opt(r.task),
            policy: policy_value(&r.policy)?,
            providers: (!r.providers.is_empty()).then_some(r.providers),
            request_id: opt(r.request_id),
            idle_timeout_s: timeout(r.idle_timeout.as_ref())?,
            image: None,
            grant: None,
        };
        let v = run(state, &req, Operation::CreateSandbox(args)).await?;
        Ok(Response::new(pb::CreateChildSandboxResponse {
            handle: s(&v, "handle"),
            depth: n(&v, "depth"),
            can_create_children: b(&v, "can_create_children"),
            repeated_request: b(&v, "repeated_request"),
            kind: s(&v, "kind"),
            lifetime: s(&v, "lifetime"),
        }))
    }

    pub(crate) async fn wait_child(
        state: &Arc<ServerState>,
        req: Request<pb::WaitChildSandboxRequest>,
    ) -> Result<Response<pb::WaitChildSandboxResponse>, Status> {
        let r = req.get_ref().clone();
        let op = Operation::Wait(WaitArgs {
            handle: r.handle,
            timeout_s: timeout(r.timeout.as_ref())?,
            for_result: None,
        });
        let v = run(state, &req, op).await?;
        Ok(Response::new(pb::WaitChildSandboxResponse {
            phase: s(&v, "phase"),
            exit_code: v
                .get("exit_code")
                .and_then(Value::as_i64)
                .and_then(|x| i32::try_from(x).ok()),
            timed_out: b(&v, "timed_out"),
        }))
    }

    pub(crate) async fn exec_child(
        state: &Arc<ServerState>,
        req: Request<pb::ExecChildSandboxRequest>,
    ) -> Result<Response<pb::ExecChildSandboxResponse>, Status> {
        let r = req.get_ref().clone();
        if r.command.is_empty() {
            return Err(Status::invalid_argument(
                "INVALID_ARGUMENT: command must not be empty",
            ));
        }
        let op = Operation::Exec(ExecArgs {
            handle: r.handle,
            command: r.command,
            timeout_s: timeout(r.timeout.as_ref())?,
        });
        let v = run(state, &req, op).await?;
        Ok(Response::new(pb::ExecChildSandboxResponse {
            exit_code: v
                .get("exit_code")
                .and_then(Value::as_i64)
                .and_then(|x| i32::try_from(x).ok())
                .unwrap_or_default(),
            stdout: s(&v, "stdout"),
            stderr: s(&v, "stderr"),
        }))
    }

    pub(crate) async fn get_child_file(
        state: &Arc<ServerState>,
        req: Request<pb::GetChildSandboxFileRequest>,
    ) -> Result<Response<pb::GetChildSandboxFileResponse>, Status> {
        let r = req.get_ref().clone();
        let v = run(
            state,
            &req,
            Operation::GetFile(FileArgs {
                handle: r.handle,
                path: r.path,
            }),
        )
        .await?;
        Ok(Response::new(pb::GetChildSandboxFileResponse {
            content: s(&v, "content"),
        }))
    }

    pub(crate) async fn stop_child(
        state: &Arc<ServerState>,
        req: Request<pb::StopChildSandboxRequest>,
    ) -> Result<Response<pb::StopChildSandboxResponse>, Status> {
        let r = req.get_ref().clone();
        let v = run(
            state,
            &req,
            Operation::Stop(HandleArgs { handle: r.handle }),
        )
        .await?;
        Ok(Response::new(pb::StopChildSandboxResponse {
            stopped: b(&v, "stopped"),
        }))
    }

    pub(crate) async fn get_limits(
        state: &Arc<ServerState>,
        req: Request<pb::GetSandboxLimitsRequest>,
    ) -> Result<Response<pb::GetSandboxLimitsResponse>, Status> {
        let v = run(state, &req, Operation::GetLimits).await?;
        Ok(Response::new(pb::GetSandboxLimitsResponse {
            has_grant: b(&v, "has_grant"),
            grant_id: s(&v, "grant_id"),
            depth: n(&v, "depth"),
            max_depth: n(&v, "max_depth"),
            children_created: n(&v, "children_created"),
            max_breadth: n(&v, "max_breadth"),
            total: n(&v, "total"),
            max_total: n(&v, "max_total"),
            can_create_children: b(&v, "can_create_children"),
        }))
    }

    fn children_of(v: &Value) -> Vec<pb::ChildSandbox> {
        v.get("children")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|c| pb::ChildSandbox {
                        handle: s(c, "handle"),
                        kind: s(c, "kind"),
                        lifetime: s(c, "lifetime"),
                        depth: n(c, "depth"),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Sandboxes list their own children; users name the parent.
    pub(crate) async fn list_children_rpc(
        state: &Arc<ServerState>,
        req: Request<pb::ListChildSandboxesRequest>,
    ) -> Result<Response<pb::ListChildSandboxesResponse>, Status> {
        let principal = crate::grpc::extract_principal(&req)?;
        let r = req.get_ref();
        let workspace = r
            .workspace_scope
            .as_ref()
            .map(|w| format!("{w:?}"))
            .unwrap_or_default();
        // The cursor is the last child's handle; children sort by handle.
        let pagination = Pagination::new(
            r.page_size,
            &r.page_token,
            "ListChildSandboxes",
            &[&r.parent, &workspace],
        )?;
        if matches!(principal, Principal::Sandbox(_)) {
            let v = run(state, &req, Operation::ListChildren).await?;
            let (children, next_page_token) =
                page(children_of(&v), |c| c.handle.as_str(), &pagination)?;
            return Ok(Response::new(pb::ListChildSandboxesResponse {
                children,
                next_page_token,
            }));
        }
        if r.parent.trim().is_empty() {
            return Err(Status::invalid_argument("parent is required"));
        }
        let authz = authorize_workspace(
            &state.store,
            &state.admin_role,
            &principal,
            selected_workspace_name(r.workspace_scope.as_ref())?,
            MinWorkspaceRole::User,
        )
        .await?;
        let parent = state
            .store
            .get_message_by_name::<Sandbox>(&authz.workspace, &r.parent)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("sandbox not found"))?;
        let v = list_children(state, parent.object_id());
        let (children, next_page_token) =
            page(children_of(&v), |c| c.handle.as_str(), &pagination)?;
        Ok(Response::new(pb::ListChildSandboxesResponse {
            children,
            next_page_token,
        }))
    }

    pub(crate) async fn list_grants(
        state: &Arc<ServerState>,
        req: Request<pb::ListSandboxGrantsRequest>,
    ) -> Result<Response<pb::ListSandboxGrantsResponse>, Status> {
        let r = req.get_ref();
        // The cursor is the last grant's id; grants sort by id.
        let pagination = Pagination::new(r.page_size, &r.page_token, "ListSandboxGrants", &[])?;
        let v = run(state, &req, Operation::GrantList).await?;
        let grants = v
            .get("grants")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|g| pb::SandboxGrant {
                        grant_id: s(g, "grant_id"),
                        holder: s(g, "holder"),
                        max_depth: n(g, "max_depth"),
                        max_breadth: n(g, "max_breadth"),
                        max_total: n(g, "max_total"),
                        total: n(g, "total"),
                        program: s(g, "program"),
                        user: s(g, "user"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (grants, next_page_token) = page(grants, |g| g.grant_id.as_str(), &pagination)?;
        Ok(Response::new(pb::ListSandboxGrantsResponse {
            grants,
            next_page_token,
        }))
    }

    pub(crate) async fn update_grant(
        state: &Arc<ServerState>,
        req: Request<pb::UpdateSandboxGrantRequest>,
    ) -> Result<Response<pb::UpdateSandboxGrantResponse>, Status> {
        let r = req.get_ref().clone();
        let op = Operation::GrantUpdate(GrantUpdateArgs {
            grant_id: r.grant_id,
            max_depth: r.max_depth,
            max_breadth: r.max_breadth,
            max_total: r.max_total,
            revoke: None,
        });
        let v = run(state, &req, op).await?;
        Ok(Response::new(pb::UpdateSandboxGrantResponse {
            grant_id: s(&v, "grant_id"),
            max_depth: n(&v, "max_depth"),
            max_breadth: n(&v, "max_breadth"),
            max_total: n(&v, "max_total"),
        }))
    }

    pub(crate) async fn create_grant(
        state: &Arc<ServerState>,
        req: Request<pb::CreateSandboxGrantRequest>,
    ) -> Result<Response<pb::CreateSandboxGrantResponse>, Status> {
        let principal = crate::grpc::extract_principal(&req)?;
        let Principal::User(user) = &principal else {
            return Err(Status::permission_denied(
                "NOT_ALLOWED: only the user can set a grant",
            ));
        };
        let r = req.get_ref();
        if r.sandbox.trim().is_empty() {
            return Err(Status::invalid_argument("sandbox is required"));
        }
        let authz = authorize_workspace(
            &state.store,
            &state.admin_role,
            &principal,
            selected_workspace_name(r.workspace_scope.as_ref())?,
            MinWorkspaceRole::User,
        )
        .await?;
        let mut parts = vec![
            format!("max_depth={}", r.max_depth),
            format!("max_breadth={}", r.max_breadth),
            format!("max_total={}", r.max_total),
            format!("program={}", r.program),
        ];
        if !r.expires.trim().is_empty() {
            parts.push(format!("expires={}", r.expires.trim()));
        }
        if !r.child_policy.trim().is_empty() {
            parts.push(format!("child_policy={}", r.child_policy.trim()));
        }
        let v = super::grant_create(state, user, &authz.workspace, &r.sandbox, &parts.join(","))
            .await
            .map_err(|e| status_of(ApiError::from(e)))?;
        Ok(Response::new(pb::CreateSandboxGrantResponse {
            grant: Some(pb::SandboxGrant {
                grant_id: s(&v, "grant_id"),
                holder: s(&v, "holder"),
                max_depth: n(&v, "max_depth"),
                max_breadth: n(&v, "max_breadth"),
                max_total: n(&v, "max_total"),
                total: n(&v, "total"),
                program: s(&v, "program"),
                user: s(&v, "user"),
            }),
        }))
    }

    pub(crate) async fn revoke_grant(
        state: &Arc<ServerState>,
        req: Request<pb::RevokeSandboxGrantRequest>,
    ) -> Result<Response<pb::RevokeSandboxGrantResponse>, Status> {
        let r = req.get_ref().clone();
        let op = Operation::GrantUpdate(GrantUpdateArgs {
            grant_id: r.grant_id,
            revoke: Some(true),
            ..Default::default()
        });
        let v = run(state, &req, op).await?;
        Ok(Response::new(pb::RevokeSandboxGrantResponse {
            revoked: b(&v, "revoked"),
        }))
    }
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

    fn policy(yaml: &str) -> SandboxPolicy {
        openshell_policy::parse_sandbox_policy(yaml).unwrap()
    }

    const MCP_AND_EXAMPLE: &str = "version: 1\nnetwork_policies:\n  gh:\n    name: gh\n    endpoints:\n      - host: api.githubcopilot.com\n        port: 443\n        path: /mcp\n        protocol: mcp\n        enforcement: enforce\n        rules:\n          - allow: { method: tools/call, tool: get_issue }\n    binaries: [{ path: /usr/local/bin/node }]\n  ex:\n    name: ex\n    endpoints:\n      - host: example.com\n        port: 443\n    binaries: [{ path: /usr/local/bin/node }]\n";

    #[tokio::test]
    async fn parent_with_mcp_rules_can_still_create_children() {
        let parent = policy(MCP_AND_EXAMPLE);
        let mut child = parent.clone();
        child.network_policies.remove("gh");
        prove(&parent, &child).await.unwrap();
    }

    #[tokio::test]
    async fn refusal_tells_the_agent_how_to_ask_for_more() {
        let parent = policy(
            "version: 1\nnetwork_policies:\n  ex:\n    name: ex\n    endpoints:\n      - host: example.com\n        port: 443\n    binaries: [{ path: /usr/local/bin/node }]\n",
        );
        let mut child = parent.clone();
        child.network_policies.get_mut("ex").unwrap().endpoints[0].host = "other.com".into();
        let err = with_create_hint(prove(&parent, &child).await.unwrap_err());
        assert_eq!(err.code, "EXCEEDS_CEILING");
        assert!(err.message.contains("http://policy.local/v1/proposals"));
    }

    const FS_PARENT: &str = "version: 1\nfilesystem_policy:\n  include_workdir: true\n  read_only: [/usr, /lib]\n  read_write: [/sandbox, /tmp]\n";

    #[tokio::test]
    async fn child_may_narrow_filesystem() {
        let parent = policy(FS_PARENT);
        let child = policy(
            "version: 1\nfilesystem_policy:\n  include_workdir: true\n  read_only: [/usr, /lib, /sandbox]\n  read_write: [/tmp]\n",
        );
        prove(&parent, &child).await.unwrap();
    }

    #[tokio::test]
    async fn child_cannot_widen_filesystem() {
        let parent = policy(FS_PARENT);
        // Write access to a path the parent only reads: a counterexample.
        let child = policy(
            "version: 1\nfilesystem_policy:\n  include_workdir: true\n  read_only: [/usr]\n  read_write: [/sandbox, /tmp, /lib]\n",
        );
        let err = prove(&parent, &child).await.unwrap_err();
        assert!(
            matches!(err.code, "EXCEEDS_CEILING" | "UNSUPPORTED_RULES"),
            "{}: {}",
            err.code,
            err.message
        );
        // A path the parent does not list: the prover cannot resolve it and
        // refuses rather than guessing.
        let child = policy(
            "version: 1\nfilesystem_policy:\n  include_workdir: true\n  read_only: [/usr, /lib]\n  read_write: [/sandbox, /tmp, /etc]\n",
        );
        assert!(prove(&parent, &child).await.is_err());
    }

    fn stored_sandbox(id: &str, policy: SandboxPolicy, parent: Option<&str>) -> Sandbox {
        let mut annotations = HashMap::new();
        if let Some(parent) = parent {
            annotations.insert(PARENT_ANNOTATION.to_string(), parent.to_string());
        }
        Sandbox {
            metadata: Some(openshell_core::proto::datamodel::v1::ObjectMeta {
                id: id.to_string(),
                name: id.to_string(),
                annotations,
                workspace: "default".to_string(),
                ..Default::default()
            }),
            spec: Some(SandboxSpec {
                policy: Some(policy),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn later_policy_writes_to_a_child_stay_within_its_parent() {
        let store = crate::persistence::test_store().await;
        let example = "version: 1\nnetwork_policies:\n  ex:\n    name: ex\n    endpoints:\n      - host: example.com\n        port: 443\n    binaries: [{ path: /usr/local/bin/node }]\n";
        let other = example.replace("example.com", "other.com");
        store
            .put_message(&stored_sandbox("p1", policy(example), None))
            .await
            .unwrap();
        store
            .put_message(&stored_sandbox("c1", policy("version: 1\n"), Some("p1")))
            .await
            .unwrap();
        // Within the parent: allowed.
        check_child_policy_write(&store, "c1", &policy(example))
            .await
            .unwrap();
        // Beyond the parent: refused.
        let err = check_child_policy_write(&store, "c1", &policy(&other))
            .await
            .unwrap_err();
        assert!(
            err.message().starts_with("EXCEEDS_CEILING"),
            "{}",
            err.message()
        );
        // The parent itself is not a child: no check.
        check_child_policy_write(&store, "p1", &policy(&other))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn child_cannot_receive_mcp_rules() {
        let parent = policy(MCP_AND_EXAMPLE);
        let err = prove(&parent, &parent).await.unwrap_err();
        assert_eq!(err.code, "UNSUPPORTED_RULES");
    }

    #[test]
    fn modelable_boundary_drops_only_unmodeled_endpoints() {
        let b = modelable_boundary(&policy(MCP_AND_EXAMPLE));
        assert!(!b.network_policies.contains_key("gh"));
        assert!(b.network_policies.contains_key("ex"));
    }

    #[test]
    fn tool_sandboxes_never_create_children() {
        assert!(may_create_children(KIND_AGENT, 1, 2));
        assert!(!may_create_children(KIND_AGENT, 2, 2));
        assert!(!may_create_children(KIND_TOOL, 1, 2));
        assert!(!may_create_children(KIND_TOOL, 1, 9));
    }

    fn lineage(id: &str, parent: Option<&str>, depth: u32, grant: &str) -> Lineage {
        Lineage {
            sandbox_id: id.into(),
            name: id.into(),
            workspace: "default".into(),
            parent_id: parent.map(str::to_string),
            depth,
            grant_id: grant.into(),
            kind: KIND_AGENT.into(),
            lifetime: "until_stopped".into(),
            children_created: 0,
            user: user("alice", &[]).identity,
            idle_timeout_ms: None,
            last_used_ms: 0,
        }
    }

    fn user(subject: &str, roles: &[&str]) -> UserPrincipal {
        UserPrincipal {
            identity: Identity {
                subject: subject.into(),
                display_name: None,
                roles: roles.iter().map(|r| (*r).to_string()).collect(),
                scopes: Vec::new(),
                provider: crate::auth::identity::IdentityProvider::LocalDev,
            },
        }
    }

    fn grant(id: &str, holder: &str, owner: &str, breadth: u32, total: u32) -> Grant {
        Grant {
            id: id.into(),
            user: user(owner, &[]).identity,
            holder_id: holder.into(),
            max_depth: 1,
            max_breadth: breadth,
            max_total: total,
            inherit_child_policy: false,
            program: "/usr/local/bin/node".into(),
            expires_at_ms: i64::MAX,
            total: 1,
            workspace: "default".into(),
        }
    }

    fn tree(breadth: u32, total: u32) -> Inner {
        let mut inner = Inner::default();
        inner
            .grants
            .insert("g".into(), grant("g", "lead", "alice", breadth, total));
        inner
            .lineage
            .insert("lead".into(), lineage("lead", None, 0, "g"));
        inner
    }

    #[test]
    fn concurrent_creates_cannot_both_take_the_last_breadth_slot() {
        // Two requests passed the early check; only one may reserve.
        let mut inner = tree(1, 10);
        assert!(matches!(
            inner.reserve("lead", KIND_AGENT, None, 0),
            Ok(Reservation::Reserved { depth: 1, .. })
        ));
        let err = inner.reserve("lead", KIND_TOOL, None, 0).unwrap_err();
        assert_eq!(err.code, "LIMIT_REACHED");
        assert!(err.message.starts_with("breadth"), "{}", err.message);
        assert_eq!(inner.grants["g"].total, 2);
        // A failed launch gives the slot back.
        inner.release("lead", "g", None);
        assert_eq!(inner.lineage["lead"].children_created, 0);
        assert_eq!(inner.grants["g"].total, 1);
        assert!(inner.reserve("lead", KIND_AGENT, None, 0).is_ok());
    }

    #[test]
    fn concurrent_creates_cannot_exceed_the_total() {
        let mut inner = tree(10, 2);
        assert!(inner.reserve("lead", KIND_AGENT, None, 0).is_ok());
        let err = inner.reserve("lead", KIND_AGENT, None, 0).unwrap_err();
        assert!(err.message.starts_with("total"), "{}", err.message);
    }

    #[test]
    fn duplicate_request_id_never_launches_a_second_child() {
        let mut inner = tree(5, 10);
        inner
            .lineage
            .insert("other".into(), lineage("other", Some("lead"), 1, "g"));
        assert!(matches!(
            inner.reserve("lead", KIND_TOOL, Some(("r1", "fp")), 0),
            Ok(Reservation::Reserved { .. })
        ));
        // A duplicate while the first is launching.
        let err = inner
            .reserve("lead", KIND_TOOL, Some(("r1", "fp")), 0)
            .unwrap_err();
        assert_eq!(err.code, "IN_PROGRESS");
        // The same request_id from another parent is a different request.
        assert!(matches!(
            inner.reserve("other", KIND_TOOL, Some(("r1", "fp")), 0),
            Ok(Reservation::Reserved { .. })
        ));
        // After the first finishes, a retry gets its child, counted once.
        inner.complete("lead", Some(("r1", "fp")), "t1");
        assert!(matches!(
            inner.reserve("lead", KIND_TOOL, Some(("r1", "fp")), 0),
            Ok(Reservation::Repeated(h)) if h == "t1"
        ));
        assert_eq!(inner.lineage["lead"].children_created, 1);
        // A failed launch frees the request_id for a real retry.
        inner.release("other", "g", Some("r1"));
        assert!(
            inner
                .reserve("other", KIND_TOOL, Some(("r1", "fp")), 0)
                .is_ok()
        );
    }

    #[test]
    fn request_id_reused_with_different_arguments_is_refused() {
        let mut inner = tree(5, 10);
        assert!(
            inner
                .reserve("lead", KIND_TOOL, Some(("r1", "fp-a")), 0)
                .is_ok()
        );
        let err = inner
            .reserve("lead", KIND_TOOL, Some(("r1", "fp-b")), 0)
            .unwrap_err();
        assert_eq!(err.code, "INVALID_ARGUMENT", "{}", err.message);
        inner.complete("lead", Some(("r1", "fp-a")), "t1");
        let err = inner
            .reserve("lead", KIND_TOOL, Some(("r1", "fp-b")), 0)
            .unwrap_err();
        assert_eq!(err.code, "INVALID_ARGUMENT");
        assert_eq!(inner.lineage["lead"].children_created, 1);
    }

    #[test]
    fn fingerprint_ignores_request_id_and_key_order() {
        let a = json!({"name": "w", "command": ["true"], "request_id": "r1"});
        let b = json!({"command": ["true"], "request_id": "r2", "name": "w"});
        assert_eq!(request_fingerprint(&a), request_fingerprint(&b));
        let c = json!({"name": "w", "command": ["false"], "request_id": "r1"});
        assert_ne!(request_fingerprint(&a), request_fingerprint(&c));
    }

    #[test]
    fn reservation_rechecks_a_revoked_or_expired_grant() {
        let mut inner = tree(5, 10);
        inner.grants.get_mut("g").unwrap().expires_at_ms = 10;
        let err = inner.reserve("lead", KIND_AGENT, None, 11).unwrap_err();
        assert_eq!(err.message, "grant expired");
        inner.grants.remove("g");
        let err = inner.reserve("lead", KIND_AGENT, None, 0).unwrap_err();
        assert_eq!(err.message, "grant revoked");
        assert_eq!(inner.lineage["lead"].children_created, 0);
    }

    #[tokio::test]
    async fn only_the_owner_can_see_change_or_revoke_a_grant() {
        let mut state = crate::grpc::test_support::test_server_state().await;
        // With no admin role every user is a platform admin; name one.
        Arc::get_mut(&mut state).unwrap().admin_role = "openshell-admin".into();
        state
            .children
            .lock()
            .grants
            .insert("g".into(), grant("g", "lead", "alice", 2, 3));
        let alice = user("alice", &["openshell-user"]);
        let mallory = user("mallory", &["openshell-user"]);
        let as_principal = |u: &UserPrincipal| Principal::User(u.clone());

        let listed = grant_list(&state, &as_principal(&mallory), &mallory).await;
        assert_eq!(listed["grants"].as_array().unwrap().len(), 0);
        for args in [
            json!({"grant_id": "g", "revoke": true}),
            json!({"grant_id": "g", "max_total": 99}),
        ] {
            let err = grant_update(&state, &as_principal(&mallory), &mallory, &args)
                .await
                .unwrap_err();
            assert_eq!(err.code, "NOT_ALLOWED");
        }
        let g = state.children.grant("g").expect("grant survives");
        assert_eq!(g.max_total, 3);

        let listed = grant_list(&state, &as_principal(&alice), &alice).await;
        assert_eq!(listed["grants"].as_array().unwrap().len(), 1);
        grant_update(
            &state,
            &as_principal(&alice),
            &alice,
            &json!({"grant_id": "g", "revoke": true}),
        )
        .await
        .unwrap();
        assert!(state.children.grant("g").is_none());

        // A platform admin may manage any grant.
        state
            .children
            .lock()
            .grants
            .insert("g2".into(), grant("g2", "lead", "alice", 2, 3));
        let admin = user("root", &["openshell-admin"]);
        let listed = grant_list(&state, &as_principal(&admin), &admin).await;
        assert_eq!(listed["grants"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_revoked_grant_can_be_replaced_for_the_running_tree() {
        let state = crate::grpc::test_support::test_server_state().await;
        state
            .store
            .put_message(&stored_sandbox("lead", policy("version: 1\n"), None))
            .await
            .unwrap();
        {
            let mut inner = state.children.lock();
            let mut g = grant("g", "lead", "alice", 2, 3);
            g.total = 2;
            inner.grants.insert("g".into(), g);
            inner
                .lineage
                .insert("lead".into(), lineage("lead", None, 0, "g"));
            inner
                .lineage
                .insert("w1".into(), lineage("w1", Some("lead"), 1, "g"));
        }
        let alice = user("alice", &["openshell-user"]);
        let spec = "max_depth=2,max_breadth=2,max_total=5,program=/usr/local/bin/node";
        let err = grant_create(&state, &alice, "default", "lead", spec)
            .await
            .unwrap_err();
        assert_eq!(err.code, "ALREADY_EXISTS");

        state.children.lock().grants.remove("g");
        let v = grant_create(&state, &alice, "default", "lead", spec)
            .await
            .unwrap();
        let new_id = v["grant_id"].as_str().unwrap().to_string();
        assert_ne!(new_id, "g");
        assert_eq!(v["total"], 2, "running children count toward the new total");
        assert_eq!(v["max_depth"], 2);
        let inner = state.children.lock();
        assert!(inner.lineage.values().all(|l| l.grant_id == new_id));
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
            user: user("alice", &[]).identity,
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

    fn lineage_for(id: &str, parent: Option<&str>) -> Lineage {
        Lineage {
            sandbox_id: id.into(),
            name: id.into(),
            workspace: "default".into(),
            parent_id: parent.map(str::to_string),
            depth: u32::from(parent.is_some()),
            grant_id: "g".into(),
            kind: "agent".into(),
            lifetime: "until_stopped".into(),
            children_created: 0,
            user: user("alice", &[]).identity,
            idle_timeout_ms: None,
            last_used_ms: 0,
        }
    }

    #[tokio::test]
    async fn a_child_submits_one_result_for_its_parent() {
        let state = crate::grpc::test_support::test_server_state().await;
        {
            let mut i = state.children.lock();
            i.lineage.insert("lead".into(), lineage_for("lead", None));
            i.lineage
                .insert("w1".into(), lineage_for("w1", Some("lead")));
        }
        // The top of the tree has no parent to submit to.
        let err = submit_result(&state, "lead", "x").unwrap_err();
        assert_eq!(err.code, "NOT_A_CHILD");

        let v = submit_result(&state, "w1", "LGTM, 2 nits").unwrap();
        assert_eq!(v["saved"], true);
        assert_eq!(v["repeated"], false);
        // A retry with the same text is a no-op; different text is refused.
        assert_eq!(
            submit_result(&state, "w1", "LGTM, 2 nits").unwrap()["repeated"],
            true
        );
        let err = submit_result(&state, "w1", "something else").unwrap_err();
        assert_eq!(err.code, "ALREADY_SUBMITTED");
        assert_eq!(
            state.children.result_of("w1").as_deref(),
            Some("LGTM, 2 nits")
        );

        // Stopping the child forgets its result.
        state.children.forget("w1");
        assert_eq!(state.children.result_of("w1"), None);
    }

    #[tokio::test]
    async fn a_result_has_a_size_limit() {
        let state = crate::grpc::test_support::test_server_state().await;
        state
            .children
            .lock()
            .lineage
            .insert("w1".into(), lineage_for("w1", Some("lead")));
        let big = "x".repeat(MAX_RESULT_BYTES + 1);
        assert_eq!(
            submit_result(&state, "w1", &big).unwrap_err().code,
            "TOO_LARGE"
        );
        assert_eq!(state.children.result_of("w1"), None);
    }

    #[tokio::test]
    async fn wait_returns_a_submitted_result_while_the_child_runs() {
        let state = crate::grpc::test_support::test_server_state().await;
        {
            let mut i = state.children.lock();
            i.lineage.insert("lead".into(), lineage_for("lead", None));
            i.lineage
                .insert("w1".into(), lineage_for("w1", Some("lead")));
        }
        let mut sb = stored_sandbox("w1", SandboxPolicy::default(), Some("lead"));
        sb.status = Some(openshell_core::proto::SandboxStatus {
            phase: SandboxPhase::Ready as i32,
            ..Default::default()
        });
        state.store.put_message(&sb).await.unwrap();

        // Without a result, for_result keeps waiting past READY until the timeout.
        let args = json!({"handle": "w1", "timeout_s": 1, "for_result": true});
        let v = wait_child(&state, "lead", &args).await.unwrap();
        assert_eq!(v["timed_out"], true);
        assert!(v["result"].is_null());

        submit_result(&state, "w1", "done: 391").unwrap();
        let v = wait_child(&state, "lead", &args).await.unwrap();
        assert_eq!(v["timed_out"], false);
        assert_eq!(v["result"], "done: 391");
        assert_eq!(v["phase"], "SANDBOX_PHASE_READY");

        // Only the parent can wait for it.
        state
            .children
            .lock()
            .lineage
            .insert("other".into(), lineage_for("other", None));
        let err = wait_child(&state, "other", &args).await.unwrap_err();
        assert_eq!(err.code, "NOT_YOUR_CHILD");
    }
}
