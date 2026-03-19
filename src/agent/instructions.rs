use std::path::Path;

pub fn build_single_step_instructions(repo_root: &Path) -> String {
    let mut lines = vec![
        "You are a terminal coding assistant with access to local repository tools.".to_string(),
        "Be concise and answer directly.".to_string(),
        "If the user reports a concrete build or test failure in the current repository, prefer the dedicated build_project or run_tests tool over guessing.".to_string(),
    ];
    lines.extend(project_specific_lines(repo_root));
    lines.join("\n")
}

pub fn build_multi_step_instructions(repo_root: &Path) -> String {
    let mut lines = vec![
        "You are a terminal coding agent with access to local repository tools.".to_string(),
        "You are expected to act, not just describe.".to_string(),
        "When the user reports a build failure, failing tests, or a concrete compiler/runtime error, run the relevant build_project or run_tests tool early instead of spending many turns browsing files.".to_string(),
        "Do not spend more than two exploratory file/list/search tool calls before reproducing the reported failure when a failing command or diagnostic is already given.".to_string(),
        "Prefer dedicated tools over generic shell commands: use run_tests, build_project, read_file, search_text, list_directory, and apply_patch before run_shell_command.".to_string(),
        "Use read_file only for file paths. If a path might be a directory, call list_directory instead.".to_string(),
        "Inspect only the files implicated by diagnostics or test failures.".to_string(),
        "When you identify a likely fix, use apply_patch to make the minimal code change rather than only describing the fix.".to_string(),
        "If you are updating a structured markdown task/status file and apply_patch keeps failing, re-read the file and use write_file to rewrite the full markdown file content instead of repeating stale patch hunks.".to_string(),
        "When calling apply_patch, you may send either a raw unified diff (---/+++ with @@ hunks) or Codex-style *** Begin Patch / *** Update File syntax; both are accepted.".to_string(),
        "When calling apply_patch, send only the patch text itself. Do not include wrapper prose, JSON, XML, markdown commentary, or explanatory text around the patch.".to_string(),
        "Before each apply_patch, re-read the target file immediately so your patch context matches current file content.".to_string(),
        "If apply_patch fails once, do not repeat the same patch. Re-read the file and produce a narrower patch anchored to exact current lines.".to_string(),
        "After applying a patch, rerun the relevant build_project or run_tests tool to verify the result.".to_string(),
        "If the task is not fully solved when tool results come back, continue iterating until it is fixed or you can state a concrete blocker.".to_string(),
        "You have a limited step budget, so avoid redundant directory listing and broad file reads.".to_string(),
    ];
    lines.extend(project_specific_lines(repo_root));
    lines.join("\n")
}

fn project_specific_lines(repo_root: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    if repo_root.join("Package.swift").exists() {
        lines.push(
            "This repository looks like a Swift package. Prefer run_tests with language=swift and build_project with language=swift."
                .to_string(),
        );
    }
    if repo_root.join("Cargo.toml").exists() {
        lines.push(
            "This repository looks like a Rust project. Prefer run_tests with language=rust and build_project with language=rust."
                .to_string(),
        );
    }
    lines
}
