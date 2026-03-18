use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct ReadFileArgs {
    pub path: String,
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadFileResult {
    pub path: String,
    pub content: String,
    pub was_truncated: bool,
}

pub fn run(repo_root: &Path, args: ReadFileArgs) -> Result<ReadFileResult> {
    let target = repo_root.join(&args.path);
    if !target.starts_with(repo_root) {
        bail!("read_file path escapes repo root");
    }
    let raw = fs::read(&target)
        .with_context(|| format!("failed to read file {}", target.to_string_lossy()))?;
    let max = args.max_bytes.unwrap_or(32 * 1024);
    let was_truncated = raw.len() > max;
    let kept = if was_truncated { &raw[..max] } else { &raw };
    let content = String::from_utf8_lossy(kept).to_string();
    Ok(ReadFileResult {
        path: args.path,
        content,
        was_truncated,
    })
}
