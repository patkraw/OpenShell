// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! `openshell grant ...` and `openshell sandbox children` (gateway MCP proof
//! of concept). These call the gateway's gRPC child-sandbox API; the MCP
//! endpoint calls the same rules.

use crate::tls::{TlsOptions, grpc_client};
use miette::{IntoDiagnostic, Result};
use openshell_core::proto::{
    CreateSandboxGrantRequest, ListChildSandboxesRequest, ListSandboxGrantsRequest,
    RevokeSandboxGrantRequest, UpdateSandboxGrantRequest, workspace_selector,
};

pub async fn grant_list(server: &str, tls: &TlsOptions) -> Result<()> {
    let mut client = grpc_client(server, tls).await?;
    let mut grants = Vec::new();
    let mut page_token = String::new();
    loop {
        let page = client
            .list_sandbox_grants(ListSandboxGrantsRequest {
                page_size: 0,
                page_token,
            })
            .await
            .into_diagnostic()?
            .into_inner();
        grants.extend(page.grants);
        if page.next_page_token.is_empty() {
            break;
        }
        page_token = page.next_page_token;
    }
    if grants.is_empty() {
        println!("No grants found.");
        return Ok(());
    }
    println!(
        "{:<14} {:<16} {:>5} {:>7} {:>9} {:<22} USER",
        "GRANT", "HOLDER", "DEPTH", "BREADTH", "TOTAL", "PROGRAM"
    );
    for g in grants {
        println!(
            "{:<14} {:<16} {:>5} {:>7} {:>9} {:<22} {}",
            g.grant_id,
            g.holder,
            g.max_depth,
            g.max_breadth,
            format!("{}/{}", g.total, g.max_total),
            g.program,
            g.user
        );
    }
    Ok(())
}

/// Arguments of `openshell grant create`.
pub struct GrantCreate<'a> {
    pub sandbox: &'a str,
    pub workspace: &'a str,
    pub max_depth: u32,
    pub max_breadth: u32,
    pub max_total: u32,
    pub expires: &'a str,
    pub program: &'a str,
    pub child_policy: &'a str,
}

pub async fn grant_create(server: &str, args: GrantCreate<'_>, tls: &TlsOptions) -> Result<()> {
    let mut client = grpc_client(server, tls).await?;
    let g = client
        .create_sandbox_grant(CreateSandboxGrantRequest {
            sandbox: args.sandbox.to_string(),
            workspace_scope: Some(workspace_selector(args.workspace)),
            max_depth: args.max_depth,
            max_breadth: args.max_breadth,
            max_total: args.max_total,
            expires: args.expires.to_string(),
            program: args.program.to_string(),
            child_policy: args.child_policy.to_string(),
        })
        .await
        .into_diagnostic()?
        .into_inner()
        .grant
        .unwrap_or_default();
    println!(
        "✓ Grant {} attached to {}: max_depth={} max_breadth={} max_total={} program={}",
        g.grant_id, g.holder, g.max_depth, g.max_breadth, g.max_total, g.program
    );
    Ok(())
}

pub async fn grant_update(
    server: &str,
    grant_id: &str,
    max_depth: Option<u32>,
    max_breadth: Option<u32>,
    max_total: Option<u32>,
    tls: &TlsOptions,
) -> Result<()> {
    if max_depth.is_none() && max_breadth.is_none() && max_total.is_none() {
        return Err(miette::miette!(
            "nothing to change: pass --max-depth, --max-breadth or --max-total"
        ));
    }
    let mut client = grpc_client(server, tls).await?;
    let g = client
        .update_sandbox_grant(UpdateSandboxGrantRequest {
            grant_id: grant_id.to_string(),
            max_depth,
            max_breadth,
            max_total,
        })
        .await
        .into_diagnostic()?
        .into_inner();
    println!(
        "✓ Grant {} updated: max_depth={} max_breadth={} max_total={} (applies from the next call)",
        g.grant_id, g.max_depth, g.max_breadth, g.max_total
    );
    Ok(())
}

pub async fn grant_revoke(server: &str, grant_id: &str, tls: &TlsOptions) -> Result<()> {
    let mut client = grpc_client(server, tls).await?;
    client
        .revoke_sandbox_grant(RevokeSandboxGrantRequest {
            grant_id: grant_id.to_string(),
        })
        .await
        .into_diagnostic()?;
    println!("✓ Grant {grant_id} revoked: its tree can create no more children");
    Ok(())
}

pub async fn sandbox_children(
    server: &str,
    parent: &str,
    workspace: &str,
    tls: &TlsOptions,
) -> Result<()> {
    let mut client = grpc_client(server, tls).await?;
    let mut children = Vec::new();
    let mut page_token = String::new();
    loop {
        let page = client
            .list_child_sandboxes(ListChildSandboxesRequest {
                parent: parent.to_string(),
                workspace_scope: Some(workspace_selector(workspace)),
                page_size: 0,
                page_token,
            })
            .await
            .into_diagnostic()?
            .into_inner();
        children.extend(page.children);
        if page.next_page_token.is_empty() {
            break;
        }
        page_token = page.next_page_token;
    }
    if children.is_empty() {
        println!("{parent} has no children.");
        return Ok(());
    }
    println!("{:<20} {:<6} {:<14} DEPTH", "NAME", "KIND", "LIFETIME");
    for c in children {
        println!(
            "{:<20} {:<6} {:<14} {}",
            c.handle, c.kind, c.lifetime, c.depth
        );
    }
    Ok(())
}
