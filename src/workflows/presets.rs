use clap::ValueEnum;
use std::path::Path;

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum WorkflowPreset {
    RustTests,
    SwiftBuild,
    ReviewChanged,
}

pub fn apply_preset_prompt(preset: &WorkflowPreset, prompt: &str) -> String {
    match preset {
        WorkflowPreset::RustTests => {
            format!(
                "Rust test-fix workflow.\n1) Use the run_tests tool with language=rust early.\n2) Inspect only the failing modules or files implicated by diagnostics.\n3) When you identify the fix, use apply_patch to make a minimal code change.\n4) Re-run run_tests to verify.\n5) Continue until tests pass or a concrete blocker is identified.\n\nUser task: {prompt}"
            )
        }
        WorkflowPreset::SwiftBuild => {
            format!(
                "Swift build/test fix workflow.\n1) Use the run_tests tool with language=swift early. If the problem is compile-only or tests are unavailable, use build_project tool with language=swift.\n2) Inspect only the files implicated by diagnostics.\n3) When you identify the fix, use apply_patch to make a minimal code change.\n4) Re-run run_tests or build_project to verify.\n5) Continue until the failure is fixed or a concrete blocker is identified.\n\nUser task: {prompt}"
            )
        }
        WorkflowPreset::ReviewChanged => {
            format!(
                "Code review workflow.\n1) Run git diff --unified=3.\n2) Identify risks and regressions.\n3) Return findings by severity.\n\nUser task: {prompt}"
            )
        }
    }
}

pub fn infer_preset_for_prompt(repo_root: &Path, prompt: &str) -> Option<WorkflowPreset> {
    if !looks_like_build_or_test_fix(prompt) {
        return None;
    }

    if repo_root.join("Package.swift").exists() {
        return Some(WorkflowPreset::SwiftBuild);
    }
    if repo_root.join("Cargo.toml").exists() {
        return Some(WorkflowPreset::RustTests);
    }
    None
}

fn looks_like_build_or_test_fix(prompt: &str) -> bool {
    let lower = prompt.to_ascii_lowercase();
    [
        "fails to build",
        "build fails",
        "failing build",
        "failing test",
        "failing tests",
        "tests fail",
        "test fails",
        "swift test",
        "cargo test",
        "swift build",
        "cargo build",
        "compile error",
        "compiler error",
        "cannot find",
        "not in scope",
        "no such module",
        "error:",
        "fix the build",
        "fix failing tests",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn infers_swift_build_preset_for_swift_failure_prompt() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Package.swift"), "// swift-tools-version:5.3\n")
            .expect("write Package.swift");
        let preset = infer_preset_for_prompt(
            dir.path(),
            "swift test fails with cannot find 'Application' in scope",
        );
        assert_eq!(preset, Some(WorkflowPreset::SwiftBuild));
    }

    #[test]
    fn infers_rust_tests_preset_for_rust_failure_prompt() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write Cargo.toml");
        let preset = infer_preset_for_prompt(
            dir.path(),
            "cargo test fails in this crate, please fix the failing tests",
        );
        assert_eq!(preset, Some(WorkflowPreset::RustTests));
    }
}
