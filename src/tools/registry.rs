use crate::tools::apply_patch::{ApplyPatchArgs, run as run_apply_patch};
use crate::tools::git_diff::{GitDiffArgs, run as run_git_diff};
use crate::tools::git_status::run as run_git_status;
use crate::tools::list_directory::{ListDirectoryArgs, run as run_list_directory};
use crate::tools::read_file::{ReadFileArgs, run as run_read_file};
use crate::tools::run_shell_command::{RunShellCommandArgs, run as run_shell_command};
use crate::tools::search_text::{SearchTextArgs, run as run_search_text};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ToolRegistry {
    repo_root: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallResult {
    pub name: String,
    pub result: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallRequest {
    pub name: String,
    pub arguments: Value,
}

impl ToolRegistry {
    pub fn new(repo_root: impl AsRef<Path>) -> Self {
        Self {
            repo_root: repo_root.as_ref().to_path_buf(),
        }
    }

    pub fn execute(&self, call: ToolCallRequest) -> Result<ToolCallResult> {
        let result = match call.name.as_str() {
            "read_file" => {
                let args: ReadFileArgs = serde_json::from_value(call.arguments)
                    .context("invalid read_file arguments")?;
                serde_json::to_value(run_read_file(&self.repo_root, args)?)?
            }
            "list_directory" => {
                let args: ListDirectoryArgs = serde_json::from_value(call.arguments)
                    .context("invalid list_directory arguments")?;
                serde_json::to_value(run_list_directory(&self.repo_root, args)?)?
            }
            "search_text" => {
                let args: SearchTextArgs = serde_json::from_value(call.arguments)
                    .context("invalid search_text arguments")?;
                serde_json::to_value(run_search_text(&self.repo_root, args)?)?
            }
            "run_shell_command" => {
                let args: RunShellCommandArgs = serde_json::from_value(call.arguments)
                    .context("invalid run_shell_command arguments")?;
                serde_json::to_value(run_shell_command(&self.repo_root, args)?)?
            }
            "apply_patch" => {
                let args: ApplyPatchArgs = serde_json::from_value(call.arguments)
                    .context("invalid apply_patch arguments")?;
                serde_json::to_value(run_apply_patch(&self.repo_root, args)?)?
            }
            "git_status" => serde_json::to_value(run_git_status(&self.repo_root)?)?,
            "git_diff" => {
                let args: GitDiffArgs =
                    serde_json::from_value(call.arguments).context("invalid git_diff arguments")?;
                serde_json::to_value(run_git_diff(&self.repo_root, args)?)?
            }
            _ => bail!("unsupported tool: {}", call.name),
        };

        Ok(ToolCallResult {
            name: call.name,
            result,
        })
    }
}
