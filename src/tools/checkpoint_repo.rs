use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Deserialize)]
pub struct CheckpointRepoArgs {
    pub approved: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRepoResult {
    pub approval_required: bool,
    pub created: bool,
    pub path: Option<String>,
    pub stderr: String,
}

pub fn run(repo_root: &Path, args: CheckpointRepoArgs) -> Result<CheckpointRepoResult> {
    if !args.approved.unwrap_or(false) {
        return Ok(CheckpointRepoResult {
            approval_required: true,
            created: false,
            path: None,
            stderr: "checkpoint creation requires approval".to_string(),
        });
    }
    let home = std::env::var_os("HOME").context("HOME not set")?;
    let dir = std::path::PathBuf::from(home).join(".local/share/grok-agent/checkpoints");
    fs::create_dir_all(&dir)?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("checkpoint-{ts}.patch"));
    let output = Command::new("git")
        .arg("diff")
        .arg("--unified=3")
        .current_dir(repo_root)
        .output()?;
    fs::write(&path, output.stdout)?;
    Ok(CheckpointRepoResult {
        approval_required: false,
        created: true,
        path: Some(path.to_string_lossy().to_string()),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}
