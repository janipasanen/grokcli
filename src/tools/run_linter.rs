use crate::tools::run_shell_command::{RunShellCommandArgs, RunShellCommandResult, run};
use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct RunLinterArgs {
    pub language: Option<String>,
    pub approved: Option<bool>,
}

pub fn run_tool(repo_root: &Path, args: RunLinterArgs) -> Result<RunShellCommandResult> {
    let language = args.language.unwrap_or_else(|| "rust".to_string());
    let command = if language.eq_ignore_ascii_case("swift") {
        "swift format lint".to_string()
    } else {
        "cargo clippy".to_string()
    };
    run(
        repo_root,
        RunShellCommandArgs {
            command,
            working_directory: Some(".".to_string()),
            timeout_seconds: Some(600),
            approved: args.approved,
        },
    )
}
