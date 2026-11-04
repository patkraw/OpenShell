// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Background jobs for the `exec` and `wait` tools: one job per child at a
//! time, its state kept only in the child, under [`JOBS_ROOT`].
//!
//! Every step is a short `ExecSandbox` running [`JOBS_SCRIPT`]. The agent's
//! command reaches the script as arguments, which the gateway shell-escapes
//! like any exec argument; it is never part of the script text.

use super::{
    Call, CapturedOutput, ExecArgs, OUTPUT_EDGE_BYTES, Refusal, ToolError, arguments_object,
    run_in_child,
};
use openshell_core::proto::ExecSandboxRequest;
use serde_json::{Value, json};
use std::time::Duration;

const JOBS_SCRIPT: &str = include_str!("jobs.sh");
/// Where jobs live in a child. The default policy lets `/tmp` be written.
const JOBS_ROOT: &str = "/tmp/openshell/jobs";
/// Limit for each short exec that starts, checks or reads a job.
const STEP_TIMEOUT: Duration = Duration::from_secs(10);
/// How long past a step's limit the tool waits for its exit event.
const STEP_GRACE: Duration = Duration::from_secs(2);
const DEFAULT_WAIT_S: u64 = 30;
const MAX_WAIT_S: u64 = 45;
const WAIT_POLL: Duration = Duration::from_secs(2);

/// A job handle, `<n>-<token>`: `n` numbers the job in its child and
/// `token` is the gateway's, so an old handle never matches a newer job.
struct JobHandle {
    n: u32,
    token: String,
}

impl JobHandle {
    fn parse(handle: &str) -> Option<Self> {
        let (n, token) = handle.split_once('-')?;
        let n_ok = (1..=9).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit());
        let token_ok = token.len() == 8
            && token
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !(n_ok && token_ok) {
            return None;
        }
        Some(Self {
            n: n.parse().ok()?,
            token: token.to_string(),
        })
    }

    fn render(&self) -> String {
        format!("{}-{}", self.n, self.token)
    }
}

/// Start `args.command` as the child's background job and return its handle.
pub(super) async fn start(call: &Call<'_>, args: ExecArgs) -> Result<Value, ToolError> {
    let token = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let has_stdin = if args.stdin.is_empty() { "0" } else { "1" };
    let mut command = script_command("start");
    command.extend([token.clone(), has_stdin.to_string()]);
    command.extend(args.command);
    let request = ExecSandboxRequest {
        sandbox: args.name.clone(),
        command,
        workdir: args.workdir,
        stdin: args.stdin,
        ..Default::default()
    };
    let outcome = run_in_child(call, request, STEP_TIMEOUT, STEP_GRACE).await?;
    let stdout = outcome.stdout.into_bytes();
    let line = String::from_utf8_lossy(&stdout);
    let words: Vec<&str> = line.split_whitespace().collect();
    match (outcome.exit, words.as_slice()) {
        (Some((0, _)), ["started", n]) => {
            let handle = JobHandle {
                n: n.parse().map_err(|_| step_failed("start", &line))?,
                token,
            };
            Ok(json!({"job": handle.render(), "state": "running"}))
        }
        (Some((0, _)), ["busy", n, rest @ ..]) => Err(ToolError::Refused(Refusal {
            code: "failed_precondition",
            message: format!(
                "a job is already running in '{}' (job {n}-{}); wait for it, or delete the child to stop it",
                args.name,
                rest.first().unwrap_or(&"")
            ),
            retryable: false,
        })),
        (Some((0, _)), ["unusable"]) => Err(ToolError::Refused(Refusal {
            code: "failed_precondition",
            message: format!(
                "background jobs need a writable /tmp and a readable /proc in '{}'; a child uses its parent's filesystem policy",
                args.name
            ),
            retryable: false,
        })),
        _ => {
            let stderr = outcome.stderr.into_bytes();
            Err(step_failed("start", &String::from_utf8_lossy(&stderr)))
        }
    }
}

/// Wait up to `timeout_s` for a job, checking every [`WAIT_POLL`]; a job
/// still running at the end is not an error.
pub(super) async fn wait(call: &Call<'_>, arguments: &Value) -> Result<Value, ToolError> {
    let (name, handle, timeout) = parse_wait_args(arguments)?;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let state = check(call, &name, &handle, remaining).await?;
        let reply = match state {
            JobState::Running if remaining > WAIT_POLL => {
                tokio::time::sleep(WAIT_POLL).await;
                continue;
            }
            JobState::Running => json!({
                "job": handle.render(),
                "state": "running",
                "message": "the job is still running; call wait again",
            }),
            JobState::Lost => json!({
                "job": handle.render(),
                "state": "lost",
                "message": "the job stopped without an exit code: it was killed, or the child restarted",
            }),
            JobState::Missing => {
                return Err(job_not_found(&name, &handle));
            }
            JobState::Completed {
                exit_code,
                out_bytes,
                err_bytes,
            } => {
                let (stdout, stderr) = output(call, &name, &handle, out_bytes, err_bytes).await?;
                let (stdout, stdout_truncated, stdout_binary) = stdout.render();
                let (stderr, stderr_truncated, stderr_binary) = stderr.render();
                json!({
                    "job": handle.render(),
                    "state": "completed",
                    "exit_code": exit_code,
                    "stdout": stdout,
                    "stderr": stderr,
                    "stdout_truncated": stdout_truncated,
                    "stderr_truncated": stderr_truncated,
                    "binary_output": stdout_binary || stderr_binary,
                })
            }
        };
        return Ok(reply);
    }
}

fn parse_wait_args(arguments: &Value) -> Result<(String, JobHandle, Duration), ToolError> {
    let args = arguments_object(arguments)?;
    let name = super::required_name(&args, &["name", "job", "timeout_s"])?;
    let handle = args
        .get("job")
        .and_then(Value::as_str)
        .and_then(JobHandle::parse)
        .ok_or_else(|| ToolError::invalid("'job' must be a job id returned by exec"))?;
    let timeout_s = match args.get("timeout_s") {
        None => DEFAULT_WAIT_S,
        Some(value) => value
            .as_u64()
            .filter(|timeout| (1..=MAX_WAIT_S).contains(timeout))
            .ok_or_else(|| ToolError::invalid("'timeout_s' must be between 1 and 45"))?,
    };
    Ok((name, handle, Duration::from_secs(timeout_s)))
}

#[derive(Debug, PartialEq, Eq)]
enum JobState {
    Running,
    Completed {
        exit_code: i32,
        out_bytes: usize,
        err_bytes: usize,
    },
    Lost,
    Missing,
}

impl JobState {
    fn parse(line: &str) -> Option<Self> {
        let words: Vec<&str> = line.split_whitespace().collect();
        Some(match words.as_slice() {
            ["running"] => Self::Running,
            ["lost"] => Self::Lost,
            ["missing"] => Self::Missing,
            ["completed", exit_code, out_bytes, err_bytes] => Self::Completed {
                exit_code: exit_code.parse().ok()?,
                out_bytes: out_bytes.parse().ok()?,
                err_bytes: err_bytes.parse().ok()?,
            },
            _ => return None,
        })
    }
}

/// One check of a job, limited to what is left of the caller's wait.
async fn check(
    call: &Call<'_>,
    name: &str,
    handle: &JobHandle,
    remaining: Duration,
) -> Result<JobState, ToolError> {
    let mut command = script_command("check");
    command.extend([handle.n.to_string(), handle.token.clone()]);
    let request = ExecSandboxRequest {
        sandbox: name.to_string(),
        command,
        ..Default::default()
    };
    let timeout = remaining.clamp(Duration::from_secs(1), STEP_TIMEOUT);
    let outcome = run_in_child(call, request, timeout, STEP_GRACE).await?;
    let stdout = outcome.stdout.into_bytes();
    let line = String::from_utf8_lossy(&stdout);
    if let Some((0, _)) = outcome.exit {
        return JobState::parse(&line).ok_or_else(|| step_failed("check", &line));
    }
    let stderr = outcome.stderr.into_bytes();
    Err(step_failed("check", &String::from_utf8_lossy(&stderr)))
}

/// The edges of a finished job's output. Only the bytes the reply keeps
/// leave the child.
async fn output(
    call: &Call<'_>,
    name: &str,
    handle: &JobHandle,
    out_bytes: usize,
    err_bytes: usize,
) -> Result<(CapturedOutput, CapturedOutput), ToolError> {
    let mut command = script_command("output");
    command.extend([
        handle.n.to_string(),
        handle.token.clone(),
        OUTPUT_EDGE_BYTES.to_string(),
    ]);
    let request = ExecSandboxRequest {
        sandbox: name.to_string(),
        command,
        ..Default::default()
    };
    let outcome = run_in_child(call, request, STEP_TIMEOUT, STEP_GRACE).await?;
    match outcome.exit {
        Some((0, _)) => Ok((
            CapturedOutput::from_edges(&outcome.stdout.into_bytes(), out_bytes),
            CapturedOutput::from_edges(&outcome.stderr.into_bytes(), err_bytes),
        )),
        // A newer job replaced it between the check and this read.
        Some((3, _)) => Err(job_not_found(name, handle)),
        _ => {
            let stderr = outcome.stderr.into_bytes();
            Err(step_failed("output", &String::from_utf8_lossy(&stderr)))
        }
    }
}

fn script_command(action: &str) -> Vec<String> {
    ["sh", "-c", JOBS_SCRIPT, "sh", action, JOBS_ROOT]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn job_not_found(name: &str, handle: &JobHandle) -> ToolError {
    ToolError::Refused(Refusal {
        code: "not_found",
        message: format!(
            "job '{}' not found in '{name}': a newer job replaced it, or the child restarted",
            handle.render()
        ),
        retryable: false,
    })
}

/// A job step that did not answer as the script does: the child's own words,
/// cut short, since they are the child's data.
fn step_failed(step: &str, detail: &str) -> ToolError {
    ToolError::Refused(Refusal {
        code: "unavailable",
        message: format!(
            "the job {step} step failed in the child: {}",
            super::truncate_at_char_boundary(detail.trim(), 200)
        ),
        retryable: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_checked_before_use() {
        let handle = JobHandle::parse("12-0a1b2c3d").unwrap();
        assert_eq!((handle.n, handle.token.as_str()), (12, "0a1b2c3d"));
        assert_eq!(handle.render(), "12-0a1b2c3d");
        for bad in [
            "",
            "12",
            "12-0A1B2C3D",
            "12-0a1b2c3",
            "-0a1b2c3d",
            "1234567890-0a1b2c3d",
            "1;rm-0a1b2c3d",
            "12-0a1b2c3d-x",
        ] {
            assert!(JobHandle::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn check_lines_are_read_exactly() {
        assert_eq!(JobState::parse("running\n"), Some(JobState::Running));
        assert_eq!(JobState::parse("lost"), Some(JobState::Lost));
        assert_eq!(JobState::parse("missing"), Some(JobState::Missing));
        // busybox may pad wc's count.
        assert_eq!(
            JobState::parse("completed 7 3   4\n"),
            Some(JobState::Completed {
                exit_code: 7,
                out_bytes: 3,
                err_bytes: 4
            })
        );
        assert_eq!(JobState::parse("completed 7 3"), None);
        assert_eq!(JobState::parse("sh: not found"), None);
    }

    #[tokio::test]
    async fn wait_refuses_bad_arguments() {
        for (arguments, message) in [
            (
                json!({"name": "w"}),
                "'job' must be a job id returned by exec",
            ),
            (
                json!({"name": "w", "job": "1-zzzzzzzz"}),
                "'job' must be a job id returned by exec",
            ),
            (
                json!({"name": "w", "job": "1-0a1b2c3d", "timeout_s": 46}),
                "'timeout_s' must be between 1 and 45",
            ),
            (
                json!({"name": "w", "job": "1-0a1b2c3d", "lines": 3}),
                "unknown argument 'lines'",
            ),
        ] {
            let Err(ToolError::Refused(refusal)) = parse_wait_args(&arguments) else {
                panic!("{arguments} must be refused");
            };
            assert_eq!(refusal.message, message);
        }
    }

    /// The script's own scenarios (`jobs_test.sh`), on the test machine's
    /// `sh`: start returns at once, running then completed, a second start
    /// refused, racing starts, a killed job and a reused pid read as lost,
    /// the claim window, capped output, stdin, quoting, unwritable root.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_job_script_passes_its_scenarios() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp");
        let output = std::process::Command::new("sh")
            .arg(dir.join("jobs_test.sh"))
            .arg(dir.join("jobs.sh"))
            .output()
            .unwrap();
        let report = String::from_utf8_lossy(&output.stdout);
        assert!(report.trim_end().ends_with("ALL OK"), "{report}");
    }
}
