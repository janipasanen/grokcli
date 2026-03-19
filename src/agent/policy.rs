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

    for blocked in ["sed -i", "perl -i", "perl -pi", "ruby -i", "ruby -pi"] {
        if lower.contains(blocked) {
            return CommandPolicyDecision::Block {
                reason: format!(
                    "shell-based file editing is blocked by policy ({blocked}); use apply_patch instead"
                ),
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

    if lower.contains(" > ")
        || lower.contains(">>")
        || lower.contains("<<")
        || lower.contains("1>")
        || lower.contains("2>")
    {
        return CommandPolicyDecision::Block {
            reason: "shell-based file editing is blocked by policy (shell redirection/heredoc); use apply_patch instead".to_string(),
        };
    }

    CommandPolicyDecision::Allow
}

fn normalize_or(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn blocks_in_place_shell_edits() {
        let dir = tempdir().expect("tempdir");
        let decision = evaluate_command("sed -i '' 's/a/b/' file.swift", dir.path(), dir.path());
        match decision {
            CommandPolicyDecision::Block { reason } => {
                assert!(reason.contains("use apply_patch instead"));
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn blocks_redirection_edits() {
        let dir = tempdir().expect("tempdir");
        let decision = evaluate_command("cat > docs/status.md << 'EOF'\nhello\nEOF", dir.path(), dir.path());
        match decision {
            CommandPolicyDecision::Block { reason } => {
                assert!(reason.contains("use apply_patch instead"));
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn blocks_output_redirection_patterns() {
        let dir = tempdir().expect("tempdir");
        let decision = evaluate_command("echo hi > file.txt", dir.path(), dir.path());
        match decision {
            CommandPolicyDecision::Block { reason } => {
                assert!(reason.contains("shell redirection/heredoc"));
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }
}
