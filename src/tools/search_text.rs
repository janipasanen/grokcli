use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Deserialize)]
pub struct SearchTextArgs {
    pub pattern: String,
    pub path: Option<String>,
    pub max_results: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchTextResult {
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub was_truncated: bool,
}

pub fn run(repo_root: &Path, args: SearchTextArgs) -> Result<SearchTextResult> {
    let rel = args.path.unwrap_or_else(|| ".".to_string());
    let target = repo_root.join(&rel);
    if !target.starts_with(repo_root) {
        bail!("search_text path escapes repo root");
    }
    let max = args.max_results.unwrap_or(200).to_string();

    let mut cmd = Command::new("rg");
    cmd.arg("--line-number")
        .arg("--no-heading")
        .arg("-m")
        .arg(&max)
        .arg(&args.pattern)
        .arg(rel.clone())
        .current_dir(repo_root);
    let output = cmd.output().context("failed to run rg")?;
    let exit_code = output.status.code().unwrap_or(-1);
    let (stdout, stdout_truncated) = truncate_output(&String::from_utf8_lossy(&output.stdout));
    let (stderr, stderr_truncated) = truncate_output(&String::from_utf8_lossy(&output.stderr));
    let line_count = stdout.lines().count();
    let was_truncated = line_count >= max.parse::<usize>().unwrap_or(200)
        || stdout_truncated
        || stderr_truncated;

    Ok(SearchTextResult {
        command: format!("rg --line-number --no-heading -m {max} '{}' {rel}", args.pattern),
        exit_code,
        stdout,
        stderr,
        was_truncated,
    })
}

fn truncate_output(text: &str) -> (String, bool) {
    const MAX_BYTES: usize = 64 * 1024;
    if text.len() <= MAX_BYTES {
        return (text.to_string(), false);
    }
    let mut truncated = text[..MAX_BYTES].to_string();
    truncated.push_str("\n...[output truncated]...");
    (truncated, true)
}
