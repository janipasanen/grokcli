use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Deserialize)]
pub struct ApplyPatchArgs {
    pub patch: String,
    pub approved: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyPatchResult {
    pub valid: bool,
    pub applied: bool,
    pub approval_required: bool,
    pub check_stderr: String,
    pub apply_stderr: String,
}

pub fn run(repo_root: &Path, args: ApplyPatchArgs) -> Result<ApplyPatchResult> {
    let (check_ok, check_stderr) = run_git_apply(repo_root, &args.patch, true)?;
    if !check_ok {
        return Ok(ApplyPatchResult {
            valid: false,
            applied: false,
            approval_required: false,
            check_stderr,
            apply_stderr: String::new(),
        });
    }

    if !args.approved.unwrap_or(false) {
        return Ok(ApplyPatchResult {
            valid: true,
            applied: false,
            approval_required: true,
            check_stderr,
            apply_stderr: String::new(),
        });
    }

    let (apply_ok, apply_stderr) = run_git_apply(repo_root, &args.patch, false)?;
    Ok(ApplyPatchResult {
        valid: true,
        applied: apply_ok,
        approval_required: false,
        check_stderr,
        apply_stderr,
    })
}

fn run_git_apply(repo_root: &Path, patch: &str, check_only: bool) -> Result<(bool, String)> {
    let mut cmd = Command::new("git");
    cmd.arg("apply");
    if check_only {
        cmd.arg("--check");
    }
    cmd.current_dir(repo_root)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::null());

    let mut child = cmd.spawn().context("failed to spawn git apply")?;
    {
        let stdin = child.stdin.as_mut().context("failed to open stdin")?;
        stdin
            .write_all(patch.as_bytes())
            .context("failed to write patch to stdin")?;
    }
    let output = child
        .wait_with_output()
        .context("failed to wait for git apply")?;
    let ok = output.status.success();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok((ok, stderr))
}
