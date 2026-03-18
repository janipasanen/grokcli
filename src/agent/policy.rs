use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandPolicyDecision {
    Allow,
    RequireApproval { reason: String },
    Block { reason: String },
}

pub fn evaluate_command(command: &str, cwd: &Path, repo_root: &Path) -> CommandPolicyDecision {
    let normalized_cwd = normalize_or(cwd);
    let normalized_root = normalize_or(repo_root);
    if !normalized_cwd.starts_with(&normalized_root) {
        return CommandPolicyDecision::Block {
            reason: "working directory outside repo root".to_string(),
        };
    }

    let lower = command.to_lowercase();

    for blocked in ["rm -rf", "git reset --hard", "git clean -fd"] {
        if lower.contains(blocked) {
            return CommandPolicyDecision::Block {
                reason: format!("blocked command pattern: {blocked}"),
            };
        }
    }

    for approval in [
        "cargo test",
        "cargo fmt",
        "cargo clippy",
        "cargo build",
        "swift build",
        "swift test",
        "swift format",
        "npm install",
        "pnpm install",
        "yarn install",
        "git add",
        "git commit",
    ] {
        if lower.contains(approval) {
            return CommandPolicyDecision::RequireApproval {
                reason: format!("requires approval by policy: {approval}"),
            };
        }
    }

    if lower.contains(" > ") || lower.contains(">>") {
        return CommandPolicyDecision::RequireApproval {
            reason: "shell redirection requires approval".to_string(),
        };
    }

    CommandPolicyDecision::Allow
}

fn normalize_or(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
