use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
    pub approved: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteFileResult {
    pub approval_required: bool,
    pub written: bool,
    pub path: String,
    pub bytes_written: usize,
    pub stderr: String,
}

pub fn run(repo_root: &Path, args: WriteFileArgs) -> Result<WriteFileResult> {
    let target = repo_root.join(&args.path);
    if !target.starts_with(repo_root) {
        bail!("write_file path escapes repo root");
    }
    if !args.approved.unwrap_or(false) {
        return Ok(WriteFileResult {
            approval_required: true,
            written: false,
            path: args.path,
            bytes_written: 0,
            stderr: "write_file requires approval".to_string(),
        });
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create parent directories for {}",
                target.to_string_lossy()
            )
        })?;
    }
    fs::write(&target, args.content.as_bytes())
        .with_context(|| format!("failed to write file {}", target.to_string_lossy()))?;
    Ok(WriteFileResult {
        approval_required: false,
        written: true,
        path: args.path,
        bytes_written: args.content.len(),
        stderr: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn write_file_requires_approval() -> Result<()> {
        let dir = tempdir()?;
        let result = run(
            dir.path(),
            WriteFileArgs {
                path: "docs/project-tasks.md".to_string(),
                content: "# tasks\n".to_string(),
                approved: Some(false),
            },
        )?;
        assert!(result.approval_required);
        assert!(!result.written);
        Ok(())
    }

    #[test]
    fn write_file_writes_content() -> Result<()> {
        let dir = tempdir()?;
        let result = run(
            dir.path(),
            WriteFileArgs {
                path: "docs/project-tasks.md".to_string(),
                content: "# tasks\n".to_string(),
                approved: Some(true),
            },
        )?;
        assert!(result.written);
        assert_eq!(
            fs::read_to_string(dir.path().join("docs/project-tasks.md"))?,
            "# tasks\n"
        );
        Ok(())
    }
}
