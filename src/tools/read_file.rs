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
    if target.is_dir() {
        bail!(
            "read_file path is a directory: {}. Use list_directory for directories",
            args.path
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn rejects_directory_paths_with_clear_message() -> Result<()> {
        let dir = tempdir()?;
        fs::create_dir_all(dir.path().join("Sources/App/Models"))?;

        let err = run(
            dir.path(),
            ReadFileArgs {
                path: "Sources/App/Models".to_string(),
                max_bytes: None,
            },
        )
        .expect_err("directory read should fail");

        let message = err.to_string();
        assert!(message.contains("path is a directory"));
        assert!(message.contains("Use list_directory"));
        Ok(())
    }
}
