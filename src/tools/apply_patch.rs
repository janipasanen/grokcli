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
    if let Some(message) = unsupported_patch_format_message(&args.patch) {
        return Ok(ApplyPatchResult {
            valid: false,
            applied: false,
            approval_required: false,
            check_stderr: message,
            apply_stderr: String::new(),
        });
    }

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

fn unsupported_patch_format_message(patch: &str) -> Option<String> {
    let uses_codex_patch_format = patch.lines().any(|line| {
        line.starts_with("*** Begin Patch")
            || line.starts_with("*** End Patch")
            || line.starts_with("*** Update File:")
            || line.starts_with("*** Add File:")
            || line.starts_with("*** Delete File:")
            || line.starts_with("*** Move to:")
    });
    if !uses_codex_patch_format {
        return None;
    }

    Some(
        "unsupported patch format: apply_patch expects a raw unified diff compatible with git apply, with file headers like `---` and `+++` plus `@@` hunk markers. Do not send the Codex-style `*** Begin Patch` / `*** Update File` format.".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn rejects_codex_style_patch_with_clear_message() -> Result<()> {
        let dir = tempdir()?;
        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
*** Begin Patch
*** Update File: demo.txt
@@
-old
+new
*** End Patch
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(!result.valid);
        assert!(!result.applied);
        assert!(result.check_stderr.contains("unsupported patch format"));
        assert!(result.check_stderr.contains("raw unified diff"));
        Ok(())
    }
}
