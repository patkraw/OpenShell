// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Gateway MCP door (proof of concept).
//!
//! The workload reaches `http://openshell.local/mcp`. The supervisor checks the
//! calling program against the sandbox's network policy (the "door rule" the
//! gateway writes from a grant), then forwards the JSON-RPC body to the
//! gateway's `/mcp` endpoint with this sandbox's own gateway token. The agent
//! never holds a credential.

use crate::policy_local::PolicyLocalContext;
use miette::{IntoDiagnostic, Result};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tracing::{info, warn};

pub const DOOR_HOST: &str = "openshell.local";
pub const DOOR_PATH: &str = "/mcp";

/// Whether a `Host` header value names the door.
#[must_use]
pub fn is_door_host(host: &str) -> bool {
    let host = host.trim();
    host.eq_ignore_ascii_case(DOOR_HOST) || host.eq_ignore_ascii_case("openshell.local:80")
}

/// Host header of a raw HTTP/1.1 request head.
#[must_use]
pub fn request_host(request_head: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(request_head);
    head.split("\r\n")
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("host"))
        .map(|(_, value)| value.trim().to_string())
}

fn header_value(request_head: &[u8], wanted: &str) -> Option<String> {
    let head = String::from_utf8_lossy(request_head);
    head.split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
        .map(|(_, value)| value.trim().to_string())
}

async fn respond<S: AsyncWrite + Unpin>(client: &mut S, status: u16, reason: &str, ctype: &str, body: &[u8]) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    client.write_all(head.as_bytes()).await.into_diagnostic()?;
    client.write_all(body).await.into_diagnostic()?;
    client.flush().await.into_diagnostic()
}

/// Serve one door request. `program_allowed` is the result of checking the
/// calling binary against the network policy for `openshell.local:80`.
pub async fn handle<S>(
    ctx: &PolicyLocalContext,
    program_allowed: std::result::Result<(), String>,
    method: &str,
    target: &str,
    initial_request: &[u8],
    client: &mut S,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if let Err(reason) = program_allowed {
        warn!(%reason, "gateway MCP door: denied (no door rule for this program)");
        let body = serde_json::json!({"error": "door_denied", "detail": reason}).to_string();
        return respond(client, 403, "Forbidden", "application/json", body.as_bytes()).await;
    }
    let path = target.split('?').next().unwrap_or(target);
    if path != DOOR_PATH {
        return respond(client, 404, "Not Found", "text/plain", b"only /mcp is served here").await;
    }
    if method != "POST" {
        return respond(client, 405, "Method Not Allowed", "text/plain", b"POST only").await;
    }
    let body = crate::policy_local::read_request_body(initial_request, client).await?;
    let Some(endpoint) = ctx.gateway_endpoint() else {
        return respond(client, 503, "Service Unavailable", "text/plain", b"no gateway endpoint").await;
    };
    let Some(bearer) = ctx.gateway_bearer() else {
        return respond(client, 503, "Service Unavailable", "text/plain", b"no gateway token yet").await;
    };
    let url = format!("{}/mcp", endpoint.trim_end_matches('/'));
    let mut req = reqwest::Client::new()
        .post(&url)
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(body);
    if let Some(v) = header_value(initial_request, "mcp-protocol-version") {
        req = req.header("mcp-protocol-version", v);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "gateway MCP door: forward failed");
            return respond(client, 502, "Bad Gateway", "text/plain", b"gateway unreachable").await;
        }
    };
    let status = resp.status();
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let bytes = resp.bytes().await.into_diagnostic()?;
    info!(status = status.as_u16(), "gateway MCP door: forwarded");
    respond(client, status.as_u16(), status.canonical_reason().unwrap_or(""), &ctype, &bytes).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_door_host() {
        assert!(is_door_host("openshell.local"));
        assert!(is_door_host("OpenShell.Local:80"));
        assert!(!is_door_host("policy.local"));
    }

    #[test]
    fn reads_host_header() {
        let req = b"POST /mcp HTTP/1.1\r\nHost: openshell.local\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(request_host(req).as_deref(), Some("openshell.local"));
        assert_eq!(header_value(req, "content-length").as_deref(), Some("2"));
    }
}
