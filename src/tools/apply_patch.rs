use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::collections::HashSet;

#[derive(Debug, Clone, Deserialize)]
pub struct ApplyPatchArgs {
    pub patch: String,
    pub approved: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyPatchResult {
    pub valid: bool,
    pub applied: bool,
    pub approval_required: bool,
    pub check_stderr: String,
    pub apply_stderr: String,
}

pub fn run(repo_root: &Path, args: ApplyPatchArgs) -> Result<ApplyPatchResult> {
    let normalized_patch = match normalize_patch(&args.patch) {
        Ok(patch) => patch,
        Err(message) => {
            return Ok(ApplyPatchResult {
                valid: false,
                applied: false,
                approval_required: false,
                check_stderr: message,
                apply_stderr: String::new(),
            });
        }
    };

    let mut patch_to_apply = normalized_patch;
    let (mut check_ok, mut check_stderr) = run_git_apply(repo_root, &patch_to_apply, true)?;
    if !check_ok
        && let Some(rewritten_patch) =
            rewrite_missing_file_updates_to_additions(&patch_to_apply, &check_stderr)
    {
        let (retry_ok, retry_stderr) = run_git_apply(repo_root, &rewritten_patch, true)?;
        if retry_ok {
            patch_to_apply = rewritten_patch;
            check_ok = true;
            check_stderr = retry_stderr;
        }
    }
    if !check_ok && ensure_missing_files_exist(repo_root, &check_stderr).is_ok() {
        let (retry_ok, retry_stderr) = run_git_apply(repo_root, &patch_to_apply, true)?;
        if retry_ok {
            check_ok = true;
            check_stderr = retry_stderr;
        }
    }
    if !check_ok
        && let Some(rewritten_patch) =
            rewrite_new_file_depends_on_old_contents(&patch_to_apply, &check_stderr)
    {
        let (retry_ok, retry_stderr) = run_git_apply(repo_root, &rewritten_patch, true)?;
        if retry_ok {
            patch_to_apply = rewritten_patch;
            check_ok = true;
            check_stderr = retry_stderr;
        }
    }

    if !check_ok {
        return Ok(ApplyPatchResult {
            valid: false,
            applied: false,
            approval_required: false,
            check_stderr,
            apply_stderr: String::new(),
        });
    }

    if !args.approved.unwrap_or(false) {
        return Ok(ApplyPatchResult {
            valid: true,
            applied: false,
            approval_required: true,
            check_stderr,
            apply_stderr: String::new(),
        });
    }

    let (apply_ok, apply_stderr) = run_git_apply(repo_root, &patch_to_apply, false)?;
    Ok(ApplyPatchResult {
        valid: true,
        applied: apply_ok,
        approval_required: false,
        check_stderr,
        apply_stderr,
    })
}

fn run_git_apply(repo_root: &Path, patch: &str, check_only: bool) -> Result<(bool, String)> {
    let (ok, stderr) = run_git_apply_with_args(repo_root, patch, check_only, false)?;
    if ok {
        return Ok((ok, stderr));
    }

    if stderr.contains("patch does not apply") || stderr.contains("patch failed:") {
        let skip_three_way = patch_targets_from_unified_diff(patch)
            .into_iter()
            .any(|target| !is_path_tracked(repo_root, &target).unwrap_or(false));
        let (retry_ok, retry_stderr) = if skip_three_way {
            (false, "3way-retry skipped: target file not tracked in index".to_string())
        } else {
            run_git_apply_with_args(repo_root, patch, check_only, true)?
        };
        if retry_ok {
            return Ok((true, retry_stderr));
        }
        let (patch_ok, patch_stderr) = run_patch_utility(repo_root, patch, check_only)?;
        if patch_ok {
            return Ok((true, patch_stderr));
        }
        let merged = if retry_stderr.trim().is_empty() {
            if patch_stderr.trim().is_empty() {
                stderr
            } else {
                format!("{stderr}\npatch-fallback:\n{patch_stderr}")
            }
        } else if stderr.trim().is_empty() {
            if patch_stderr.trim().is_empty() {
                retry_stderr
            } else {
                format!("{retry_stderr}\npatch-fallback:\n{patch_stderr}")
            }
        } else {
            if patch_stderr.trim().is_empty() {
                format!("{stderr}\n3way-retry:\n{retry_stderr}")
            } else {
                format!("{stderr}\n3way-retry:\n{retry_stderr}\npatch-fallback:\n{patch_stderr}")
            }
        };
        return Ok((false, merged));
    }

    Ok((ok, stderr))
}

fn run_git_apply_with_args(
    repo_root: &Path,
    patch: &str,
    check_only: bool,
    three_way: bool,
) -> Result<(bool, String)> {
    let mut cmd = Command::new("git");
    cmd.arg("apply");
    cmd.arg("--recount");
    cmd.arg("--ignore-space-change");
    cmd.arg("--ignore-whitespace");
    if three_way {
        cmd.arg("--3way");
    }
    if check_only {
        cmd.arg("--check");
    }
    cmd.current_dir(repo_root)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::null());

    let mut child = cmd.spawn().context("failed to spawn git apply")?;
    {
        let stdin = child.stdin.as_mut().context("failed to open stdin")?;
        stdin
            .write_all(patch.as_bytes())
            .context("failed to write patch to stdin")?;
    }
    let output = child
        .wait_with_output()
        .context("failed to wait for git apply")?;
    let ok = output.status.success();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok((ok, stderr))
}

fn run_patch_utility(repo_root: &Path, patch: &str, check_only: bool) -> Result<(bool, String)> {
    let mut combined_errors = Vec::new();
    for strip in ["-p1", "-p0", "-p2"] {
        let (ok, stderr) = run_patch_with_strip(repo_root, patch, check_only, strip)?;
        if ok {
            return Ok((true, stderr));
        }
        if !stderr.trim().is_empty() {
            combined_errors.push(format!("{strip}: {stderr}"));
        }
    }
    Ok((false, combined_errors.join("\n")))
}

fn run_patch_with_strip(
    repo_root: &Path,
    patch: &str,
    check_only: bool,
    strip_level: &str,
) -> Result<(bool, String)> {
    let mut cmd = Command::new("patch");
    if check_only {
        cmd.arg("--dry-run");
    }
    cmd.arg("--batch")
        .arg("--forward")
        .arg("--fuzz=10")
        .arg(strip_level)
        .current_dir(repo_root)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped());

    let mut child = cmd.spawn().context("failed to spawn patch utility")?;
    {
        let stdin = child.stdin.as_mut().context("failed to open patch stdin")?;
        stdin
            .write_all(patch.as_bytes())
            .context("failed to write patch utility stdin")?;
    }
    let output = child
        .wait_with_output()
        .context("failed to wait for patch utility")?;
    let ok = output.status.success();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let combined = if stderr.trim().is_empty() {
        stdout
    } else if stdout.trim().is_empty() {
        stderr
    } else {
        format!("{stderr}\n{stdout}")
    };
    Ok((ok, combined))
}

fn normalize_patch(patch: &str) -> std::result::Result<String, String> {
    let patch = sanitize_patch_input(patch);
    if patch.trim().is_empty() {
        return Err(
            "unrecognized input: apply_patch expected a patch body in unified diff or Codex patch format"
                .to_string(),
        );
    }
    if patch.lines().any(|line| line.starts_with("*** Begin Patch")) {
        return codex_patch_to_unified_diff(&patch);
    }
    if !looks_like_patch_payload(&patch) {
        return Err(
            "unrecognized input: apply_patch expected a patch body in unified diff or Codex patch format"
                .to_string(),
        );
    }
    Ok(normalize_unified_diff_hunks(&patch))
}

fn codex_patch_to_unified_diff(patch: &str) -> std::result::Result<String, String> {
    let mut lines = patch.lines().peekable();
    match lines.next() {
        Some(line) if line.starts_with("*** Begin Patch") => {}
        _ => {
            return Err(
                "invalid Codex patch format: missing `*** Begin Patch` header".to_string(),
            );
        }
    }

    let mut unified = String::new();
    while let Some(line) = lines.next() {
        if line.starts_with("*** End Patch") {
            break;
        }
        if line.starts_with("*** End of File") {
            continue;
        }
        if line.is_empty() {
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File: ") {
            append_codex_update_section(&mut unified, path, &mut lines)?;
            continue;
        }

        if let Some(path) = line
            .strip_prefix("*** Add File: ")
            .or_else(|| line.strip_prefix("*** New File: "))
        {
            let mut section = Vec::new();
            while let Some(next) = lines.peek().copied() {
                if next.starts_with("*** ") {
                    break;
                }
                section.push(normalize_add_file_line(next));
                let _ = lines.next();
            }
            let additions = section.len();
            let hunk_header = if additions == 0 {
                "@@ -0,0 +0,0 @@".to_string()
            } else if additions == 1 {
                "@@ -0,0 +1 @@".to_string()
            } else {
                format!("@@ -0,0 +1,{additions} @@")
            };

            let _ = writeln!(unified, "diff --git a/{path} b/{path}");
            let _ = writeln!(unified, "new file mode 100644");
            let _ = writeln!(unified, "--- /dev/null");
            let _ = writeln!(unified, "+++ b/{path}");
            let _ = writeln!(unified, "{hunk_header}");
            for add_line in section {
                let _ = writeln!(unified, "{add_line}");
            }
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            return Err(format!(
                "unsupported Codex patch operation for `{path}`: delete-file patches are not yet supported"
            ));
        }

        if looks_like_path_line(line) {
            append_codex_update_section(&mut unified, line, &mut lines)?;
            continue;
        }

        return Err(format!("invalid Codex patch format line: `{line}`"));
    }

    if unified.trim().is_empty() {
        return Err("invalid Codex patch format: no file operations found".to_string());
    }

    Ok(unified)
}

fn append_codex_update_section<'a, I>(
    out: &mut String,
    path: &str,
    lines: &mut std::iter::Peekable<I>,
) -> std::result::Result<(), String>
where
    I: Iterator<Item = &'a str>,
{
    let mut new_path = path.to_string();
    if let Some(next) = lines.peek().copied()
        && let Some(moved_to) = next.strip_prefix("*** Move to: ")
    {
        new_path = moved_to.to_string();
        let _ = lines.next();
    }
    let mut section = Vec::new();
    while let Some(next) = lines.peek().copied() {
        if next.starts_with("*** ") {
            break;
        }
        if looks_like_embedded_diff_header(next) {
            let _ = lines.next();
            continue;
        }
        section.push(normalize_hunk_line(next));
        let _ = lines.next();
    }
    if section.is_empty() {
        return Err(format!(
            "invalid Codex patch format for `{path}`: empty update section"
        ));
    }
    let section = normalize_codex_hunks_or_wrap(&section);
    let _ = writeln!(out, "diff --git a/{path} b/{new_path}");
    let _ = writeln!(out, "--- a/{path}");
    let _ = writeln!(out, "+++ b/{new_path}");
    for hunk_line in section {
        let _ = writeln!(out, "{hunk_line}");
    }
    Ok(())
}

fn looks_like_path_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains(" ") {
        return false;
    }
    trimmed.contains('/') || trimmed.ends_with(".swift") || trimmed.ends_with(".leaf")
}

fn looks_like_embedded_diff_header(line: &str) -> bool {
    line.starts_with("diff --git ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("index ")
}

fn sanitize_patch_input(input: &str) -> String {
    let mut text = input.trim().to_string();
    if let Some(stripped) = strip_markdown_fence(&text) {
        text = stripped;
    }

    if let Some(idx) = text.find("*** Begin Patch") {
        if idx > 0 {
            text = text[idx..].to_string();
        }
        return text;
    }

    if let Some(idx) = text.find("\ndiff --git ") {
        text = text[idx + 1..].to_string();
    } else if text.starts_with("diff --git ") {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        return text;
    } else if let Some(idx) = text.find("\n--- ") {
        text = text[idx + 1..].to_string();
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn looks_like_patch_payload(input: &str) -> bool {
    let trimmed = input.trim_start();
    trimmed.starts_with("diff --git ")
        || trimmed.starts_with("--- ")
        || trimmed.starts_with("*** Begin Patch")
}

fn strip_markdown_fence(input: &str) -> Option<String> {
    if !input.starts_with("```") {
        return None;
    }
    let after_open = input[3..].find('\n')?;
    let content_start = 3 + after_open + 1;
    let rest = &input[content_start..];
    let close = rest.rfind("\n```")?;
    Some(rest[..close].to_string())
}

fn rewrite_missing_file_updates_to_additions(patch: &str, check_stderr: &str) -> Option<String> {
    let missing_files = extract_missing_file_paths(check_stderr);
    if missing_files.is_empty() {
        return None;
    }

    let missing: HashSet<&str> = missing_files.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    let mut modified = false;
    let mut pending_new_file_mode_for: Option<String> = None;

    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            let mut parts = rest.split_whitespace();
            if let (Some(a_path), Some(b_path_part)) = (parts.next(), parts.next()) {
                let b_path = b_path_part.strip_prefix("b/").unwrap_or(b_path_part);
                if missing.contains(a_path) || missing.contains(b_path) {
                    pending_new_file_mode_for = Some(a_path.to_string());
                } else {
                    pending_new_file_mode_for = None;
                }
            }
            out.push(line.to_string());
            continue;
        }

        if line.starts_with("new file mode ") {
            pending_new_file_mode_for = None;
            out.push(line.to_string());
            continue;
        }

        if let Some(path) = line.strip_prefix("--- a/") {
            if missing.contains(path) {
                if pending_new_file_mode_for.is_some() {
                    out.push("new file mode 100644".to_string());
                    pending_new_file_mode_for = None;
                }
                out.push("--- /dev/null".to_string());
                modified = true;
                continue;
            }
        }

        out.push(line.to_string());
    }

    if !modified {
        return None;
    }

    let mut normalized = out.join("\n");
    if !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    Some(normalized)
}

fn extract_missing_file_paths(stderr: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in stderr.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("error: ")
            && let Some(path) = rest.strip_suffix(": No such file or directory")
            && !paths.iter().any(|existing| existing == path)
        {
            paths.push(path.to_string());
        }
    }
    paths
}

fn ensure_missing_files_exist(repo_root: &Path, stderr: &str) -> Result<()> {
    for path in extract_missing_file_paths(stderr) {
        if path.trim().is_empty() {
            continue;
        }
        let rel = PathBuf::from(path);
        if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            continue;
        }
        let full = repo_root.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create parent dir for {:?}", full))?;
        }
        if !full.exists() {
            fs::write(&full, b"").with_context(|| format!("failed to create {:?}", full))?;
        }
    }
    Ok(())
}

fn rewrite_new_file_depends_on_old_contents(patch: &str, check_stderr: &str) -> Option<String> {
    let affected_files = extract_new_file_depends_paths(check_stderr);
    if affected_files.is_empty() {
        return None;
    }
    let affected: HashSet<&str> = affected_files.iter().map(String::as_str).collect();

    let mut out = String::new();
    let mut section = Vec::new();
    let mut modified = false;

    let flush_section = |section: &mut Vec<String>, out: &mut String, modified: &mut bool| {
        if section.is_empty() {
            return;
        }
        let rewritten = rewrite_new_file_section(section, &affected);
        if rewritten.1 {
            *modified = true;
        }
        out.push_str(&rewritten.0);
        section.clear();
    };

    for line in patch.lines() {
        if line.starts_with("diff --git ") && !section.is_empty() {
            flush_section(&mut section, &mut out, &mut modified);
        }
        section.push(line.to_string());
    }
    flush_section(&mut section, &mut out, &mut modified);

    if !modified {
        return None;
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

fn rewrite_new_file_section(section: &[String], affected: &HashSet<&str>) -> (String, bool) {
    if section.is_empty() {
        return (String::new(), false);
    }
    let path = detect_patch_section_path(section);
    let Some(path) = path else {
        return (section.join("\n") + "\n", false);
    };
    if !affected.contains(path.as_str()) {
        return (section.join("\n") + "\n", false);
    }

    let mut additions: Vec<String> = Vec::new();
    for line in section {
        if line.starts_with("+++ ") || line.starts_with("--- ") || line.starts_with("@@") {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            additions.push(rest.to_string());
        }
    }

    let mut out = String::new();
    let _ = writeln!(out, "diff --git a/{path} b/{path}");
    let _ = writeln!(out, "new file mode 100644");
    let _ = writeln!(out, "--- /dev/null");
    let _ = writeln!(out, "+++ b/{path}");
    let additions_count = additions.len();
    let header = match additions_count {
        0 => "@@ -0,0 +0,0 @@".to_string(),
        1 => "@@ -0,0 +1 @@".to_string(),
        _ => format!("@@ -0,0 +1,{additions_count} @@"),
    };
    let _ = writeln!(out, "{header}");
    for add in additions {
        let _ = writeln!(out, "+{add}");
    }
    (out, true)
}

fn detect_patch_section_path(section: &[String]) -> Option<String> {
    for line in section {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            return Some(rest.to_string());
        }
    }
    for line in section {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let mut parts = rest.split_whitespace();
            if let (Some(_left), Some(right)) = (parts.next(), parts.next()) {
                return Some(right.trim_start_matches("b/").to_string());
            }
        }
    }
    None
}

fn extract_new_file_depends_paths(stderr: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in stderr.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("error: new file ")
            && let Some(path) = rest.strip_suffix(" depends on old contents")
            && !paths.iter().any(|existing| existing == path)
        {
            paths.push(path.to_string());
        }
    }
    paths
}

fn patch_targets_from_unified_diff(patch: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            if path != "/dev/null" && !targets.iter().any(|p| p == path) {
                targets.push(path.to_string());
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            if path != "/dev/null" {
                let normalized = path.strip_prefix("b/").unwrap_or(path);
                if !targets.iter().any(|p| p == normalized) {
                    targets.push(normalized.to_string());
                }
            }
        }
    }
    targets
}

fn is_path_tracked(repo_root: &Path, path: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("ls-files")
        .arg("--error-unmatch")
        .arg("--")
        .arg(path)
        .current_dir(repo_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("failed to query git index")?;
    Ok(output.status.success())
}

fn normalize_unified_diff_hunks(patch: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_hunk = false;
    let mut in_file_section = false;
    let mut old_start = 1usize;
    let mut new_start = 1usize;
    let mut hunk_start_idx: Option<usize> = None;
    let mut explicit_hunk_starts: Option<(usize, usize)> = None;
    let mut old_count = 0usize;
    let mut new_count = 0usize;

    let flush_hunk_header = |out: &mut Vec<String>,
                             hunk_start_idx: Option<usize>,
                             old_start: usize,
                             explicit_hunk_starts: Option<(usize, usize)>,
                             old_count: usize,
                             new_start: usize,
                             new_count: usize| {
        if let Some(idx) = hunk_start_idx {
            let (resolved_old_start, resolved_new_start) =
                explicit_hunk_starts.unwrap_or((old_start, new_start));
            out[idx] = format!(
                "@@ -{} +{} @@",
                hunk_range(resolved_old_start, old_count),
                hunk_range(resolved_new_start, new_count)
            );
        }
    };

    for original_line in patch.lines() {
        let normalized_header = normalize_unified_header_line(original_line);
        let raw_line = normalized_header.as_str();
        let is_header = raw_line.starts_with("diff --git ")
            || raw_line.starts_with("--- ")
            || raw_line.starts_with("+++ ")
            || raw_line.starts_with("index ")
            || raw_line.starts_with("new file mode ")
            || raw_line.starts_with("deleted file mode ");

        if in_hunk && (is_header || raw_line.starts_with("@@")) {
            flush_hunk_header(
                &mut out,
                hunk_start_idx,
                old_start,
                explicit_hunk_starts,
                old_count,
                new_start,
                new_count,
            );
            old_start += old_count;
            new_start += new_count;
            old_count = 0;
            new_count = 0;
            hunk_start_idx = None;
            explicit_hunk_starts = None;
            if is_header {
                in_hunk = false;
            }
        }

        if raw_line.starts_with("+++ ") {
            in_file_section = true;
            out.push(raw_line.to_string());
            continue;
        }

        if raw_line.starts_with("@@") {
            in_hunk = true;
            explicit_hunk_starts = parse_hunk_starts(raw_line);
            let line = raw_line.trim().to_string();
            hunk_start_idx = Some(out.len());
            out.push(line);
            continue;
        }

        if in_file_section && !in_hunk && !is_header && looks_like_hunk_body_line(raw_line) {
            in_hunk = true;
            hunk_start_idx = Some(out.len());
            explicit_hunk_starts = None;
            out.push("@@".to_string());
            let normalized = normalize_hunk_line(raw_line);
            match normalized.chars().next() {
                Some(' ') => {
                    old_count += 1;
                    new_count += 1;
                }
                Some('-') => old_count += 1,
                Some('+') => new_count += 1,
                _ => {}
            }
            out.push(normalized);
            continue;
        }

        if in_hunk {
            let normalized = normalize_hunk_line(raw_line);
            match normalized.chars().next() {
                Some(' ') => {
                    old_count += 1;
                    new_count += 1;
                }
                Some('-') => old_count += 1,
                Some('+') => new_count += 1,
                _ => {}
            }
            out.push(normalized);
            continue;
        }

        out.push(raw_line.to_string());
    }

    if in_hunk {
        flush_hunk_header(
            &mut out,
            hunk_start_idx,
            old_start,
            explicit_hunk_starts,
            old_count,
            new_start,
            new_count,
        );
    }

    let mut normalized = out.join("\n");
    if !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    normalized
}

fn parse_hunk_starts(line: &str) -> Option<(usize, usize)> {
    let line = line.trim();
    if !line.starts_with("@@") {
        return None;
    }
    let parts: Vec<&str> = line.split("@@").collect();
    let body = parts.get(1)?.trim();
    let mut ranges = body.split_whitespace();
    let old_range = ranges.next()?.strip_prefix('-')?;
    let new_range = ranges.next()?.strip_prefix('+')?;
    Some((parse_hunk_start(old_range)?, parse_hunk_start(new_range)?))
}

fn parse_hunk_start(range: &str) -> Option<usize> {
    let start = range.split(',').next()?;
    start.parse::<usize>().ok()
}

fn looks_like_hunk_body_line(line: &str) -> bool {
    if line.is_empty() {
        return true;
    }
    matches!(line.chars().next(), Some(' ') | Some('+') | Some('-') | Some('\\'))
        || !line.starts_with("diff --git ")
            && !line.starts_with("--- ")
            && !line.starts_with("+++ ")
            && !line.starts_with("index ")
            && !line.starts_with("new file mode ")
            && !line.starts_with("deleted file mode ")
}

fn normalize_unified_header_line(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("diff --git ") {
        let mut parts = rest.split_whitespace();
        if let (Some(left), Some(right)) = (parts.next(), parts.next()) {
            return format!(
                "diff --git {} {}",
                ensure_diff_left_path(left),
                ensure_diff_right_path(right)
            );
        }
        return line.to_string();
    }

    if let Some(path) = line.strip_prefix("--- ") {
        if path == "/dev/null" {
            return line.to_string();
        }
        return format!("--- {}", ensure_diff_left_path(path));
    }

    if let Some(path) = line.strip_prefix("+++ ") {
        if path == "/dev/null" {
            return line.to_string();
        }
        return format!("+++ {}", ensure_diff_right_path(path));
    }

    line.to_string()
}

fn ensure_diff_left_path(path: &str) -> String {
    if path.starts_with("a/") {
        return path.to_string();
    }
    if let Some(stripped) = path.strip_prefix("b/") {
        return format!("a/{stripped}");
    }
    format!("a/{path}")
}

fn ensure_diff_right_path(path: &str) -> String {
    if path.starts_with("b/") {
        return path.to_string();
    }
    if let Some(stripped) = path.strip_prefix("a/") {
        return format!("b/{stripped}");
    }
    format!("b/{path}")
}

fn normalize_codex_hunks_or_wrap(section: &[String]) -> Vec<String> {
    if !section.iter().any(|line| line.starts_with("@@")) {
        let (old_count, new_count) = count_hunk_lines(section);
        let mut wrapped = Vec::with_capacity(section.len() + 1);
        wrapped.push(format!(
            "@@ -{} +{} @@",
            hunk_range(1, old_count),
            hunk_range(1, new_count)
        ));
        wrapped.extend(section.iter().cloned());
        return wrapped;
    }

    let mut normalized = Vec::with_capacity(section.len());
    let mut i = 0usize;
    let mut old_start = 1usize;
    let mut new_start = 1usize;

    while i < section.len() {
        let line = &section[i];
        if !line.starts_with("@@") {
            normalized.push(line.clone());
            i += 1;
            continue;
        }

        let mut j = i + 1;
        let mut old_count = 0usize;
        let mut new_count = 0usize;
        while j < section.len() && !section[j].starts_with("@@") {
            if let Some(prefix) = section[j].chars().next() {
                match prefix {
                    ' ' => {
                        old_count += 1;
                        new_count += 1;
                    }
                    '-' => old_count += 1,
                    '+' => new_count += 1,
                    _ => {}
                }
            }
            j += 1;
        }

        let has_explicit_ranges = line
            .strip_prefix("@@")
            .map(|rest| rest.contains("@@"))
            .unwrap_or(false);
        if has_explicit_ranges {
            normalized.push(line.clone());
        } else {
            normalized.push(format!(
                "@@ -{} +{} @@",
                hunk_range(old_start, old_count),
                hunk_range(new_start, new_count)
            ));
        }

        old_start += old_count;
        new_start += new_count;
        i += 1;
    }

    normalized
}

fn count_hunk_lines(lines: &[String]) -> (usize, usize) {
    let mut old_count = 0usize;
    let mut new_count = 0usize;
    for line in lines {
        if line.starts_with("\\ No newline at end of file") {
            continue;
        }
        match line.chars().next() {
            Some(' ') => {
                old_count += 1;
                new_count += 1;
            }
            Some('-') => old_count += 1,
            Some('+') => new_count += 1,
            Some(_) | None => {
                old_count += 1;
                new_count += 1;
            }
        }
    }
    (old_count, new_count)
}

fn normalize_hunk_line(line: &str) -> String {
    let line = normalize_common_escaped_content(line);
    if line.starts_with("\\ No newline at end of file") {
        return line;
    }
    match line.chars().next() {
        Some(' ') | Some('+') | Some('-') | Some('@') => line,
        _ => format!(" {line}"),
    }
}

fn normalize_add_file_line(line: &str) -> String {
    let line = normalize_common_escaped_content(line);
    if line.starts_with("\\ No newline at end of file") {
        return line;
    }
    match line.chars().next() {
        Some('+') => line,
        Some(' ') => format!("+{}", &line[1..]),
        Some('-') => format!("+{}", &line[1..]),
        _ => format!("+{line}"),
    }
}

fn normalize_common_escaped_content(line: &str) -> String {
    if line.contains("\\\"") {
        return line.replace("\\\"", "\"");
    }
    line.to_string()
}

fn hunk_range(start: usize, count: usize) -> String {
    match count {
        0 => format!("{start},0"),
        1 => start.to_string(),
        _ => format!("{start},{count}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn converts_codex_style_update_patch_and_applies() -> Result<()> {
        let dir = tempdir()?;
        fs::write(dir.path().join("demo.txt"), "old\n")?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
*** Begin Patch
*** Update File: demo.txt
@@
-old
+new
*** End Patch
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(fs::read_to_string(dir.path().join("demo.txt"))?, "new\n");
        Ok(())
    }

    #[test]
    fn rejects_unsupported_codex_delete_operation() -> Result<()> {
        let dir = tempdir()?;
        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
*** Begin Patch
*** Delete File: demo.txt
*** End Patch
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(!result.valid);
        assert!(result.check_stderr.contains("delete-file patches are not yet supported"));
        Ok(())
    }

    #[test]
    fn converts_codex_update_patch_without_explicit_hunk_markers() -> Result<()> {
        let dir = tempdir()?;
        fs::write(dir.path().join("demo.txt"), "old\n")?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
*** Begin Patch
*** Update File: demo.txt
-old
+new
*** End Patch
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(fs::read_to_string(dir.path().join("demo.txt"))?, "new\n");
        Ok(())
    }

    #[test]
    fn converts_fenced_codex_patch_and_applies() -> Result<()> {
        let dir = tempdir()?;
        fs::write(dir.path().join("demo.txt"), "old\n")?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
```diff
*** Begin Patch
*** Update File: demo.txt
-old
+new
*** End Patch
```
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(fs::read_to_string(dir.path().join("demo.txt"))?, "new\n");
        Ok(())
    }

    #[test]
    fn applies_fenced_unified_diff_patch() -> Result<()> {
        let dir = tempdir()?;
        fs::write(dir.path().join("demo.txt"), "old\n")?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
```diff
diff --git a/demo.txt b/demo.txt
--- a/demo.txt
+++ b/demo.txt
@@ -1 +1 @@
-old
+new
```
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(fs::read_to_string(dir.path().join("demo.txt"))?, "new\n");
        Ok(())
    }

    #[test]
    fn converts_codex_update_patch_with_unprefixed_lines() -> Result<()> {
        let dir = tempdir()?;
        fs::write(dir.path().join("demo.txt"), "old\n")?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
*** Begin Patch
*** Update File: demo.txt
old
new
*** End Patch
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(
            !result.valid || !result.applied,
            "unprefixed full-replacement patch should not silently produce an unsafe rewrite"
        );
        Ok(())
    }

    #[test]
    fn accepts_new_file_alias_in_codex_patch() -> Result<()> {
        let dir = tempdir()?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
*** Begin Patch
*** New File: demo.txt
+hello
*** End Patch
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(fs::read_to_string(dir.path().join("demo.txt"))?, "hello\n");
        Ok(())
    }

    #[test]
    fn normalizes_unified_diff_with_bare_hunk_header() -> Result<()> {
        let dir = tempdir()?;
        fs::write(dir.path().join("demo.txt"), "old\n")?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
diff --git a/demo.txt b/demo.txt
--- a/demo.txt
+++ b/demo.txt
@@
-old
+new
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(fs::read_to_string(dir.path().join("demo.txt"))?, "new\n");
        Ok(())
    }

    #[test]
    fn normalizes_unified_diff_without_any_hunk_header() -> Result<()> {
        let dir = tempdir()?;
        fs::write(dir.path().join("demo.txt"), "old\n")?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
diff --git a/demo.txt b/demo.txt
--- a/demo.txt
+++ b/demo.txt
-old
+new
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(fs::read_to_string(dir.path().join("demo.txt"))?, "new\n");
        Ok(())
    }

    #[test]
    fn normalizes_unified_diff_without_hunk_header_and_with_context_lines() -> Result<()> {
        let dir = tempdir()?;
        fs::write(dir.path().join("demo.txt"), "alpha\nold\nomega\n")?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
diff --git a/demo.txt b/demo.txt
--- a/demo.txt
+++ b/demo.txt
 alpha
-old
+new
 omega
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(
            fs::read_to_string(dir.path().join("demo.txt"))?,
            "alpha\nnew\nomega\n"
        );
        Ok(())
    }

    #[test]
    fn rewrites_explicit_hunk_headers_with_wrong_counts() -> Result<()> {
        let dir = tempdir()?;
        fs::write(dir.path().join("demo.txt"), "alpha\nold\nomega\n")?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
diff --git a/demo.txt b/demo.txt
--- a/demo.txt
+++ b/demo.txt
@@ -1,99 +1,99 @@
 alpha
-old
+new
 omega
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(
            fs::read_to_string(dir.path().join("demo.txt"))?,
            "alpha\nnew\nomega\n"
        );
        Ok(())
    }

    #[test]
    fn ignores_codex_end_of_file_marker() -> Result<()> {
        let dir = tempdir()?;
        fs::write(dir.path().join("demo.txt"), "old\n")?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
*** Begin Patch
*** Update File: demo.txt
@@
-old
+new
*** End of File
*** End Patch
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(fs::read_to_string(dir.path().join("demo.txt"))?, "new\n");
        Ok(())
    }

    #[test]
    fn applies_patch_with_whitespace_mismatch_in_context() -> Result<()> {
        let dir = tempdir()?;
        fs::write(dir.path().join("demo.txt"), "func a() {\n    return 1\n}\n")?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
diff --git a/demo.txt b/demo.txt
--- a/demo.txt
+++ b/demo.txt
@@ -1,3 +1,3 @@
 func a() {
-  return 1
+  return 2
 }
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(
            fs::read_to_string(dir.path().join("demo.txt"))?,
            "func a() {\n  return 2\n}\n"
        );
        Ok(())
    }

    #[test]
    fn retries_missing_update_file_as_new_file_patch() -> Result<()> {
        let dir = tempdir()?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
diff --git a/demo.txt b/demo.txt
--- a/demo.txt
+++ b/demo.txt
@@ -0,0 +1 @@
+hello
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(fs::read_to_string(dir.path().join("demo.txt"))?, "hello\n");
        Ok(())
    }

    #[test]
    fn normalizes_unified_diff_paths_without_a_b_prefixes() -> Result<()> {
        let dir = tempdir()?;
        fs::create_dir_all(dir.path().join("Resources/Views"))?;
        fs::write(
            dir.path().join("Resources/Views/page.leaf"),
            "old\n",
        )?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
diff --git Resources/Views/page.leaf Resources/Views/page.leaf
--- Resources/Views/page.leaf
+++ Resources/Views/page.leaf
@@ -1 +1 @@
-old
+new
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(
            fs::read_to_string(dir.path().join("Resources/Views/page.leaf"))?,
            "new\n"
        );
        Ok(())
    }

    #[test]
    fn retries_new_file_patch_that_depends_on_old_contents() -> Result<()> {
        let dir = tempdir()?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
diff --git a/demo.txt b/demo.txt
new file mode 100644
--- /dev/null
+++ b/demo.txt
@@ -1 +1 @@
-old
+new
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(fs::read_to_string(dir.path().join("demo.txt"))?, "new\n");
        Ok(())
    }

    #[test]
    fn accepts_codex_patch_with_bare_path_line_for_update() -> Result<()> {
        let dir = tempdir()?;
        fs::create_dir_all(dir.path().join("Resources/Views"))?;
        fs::write(
            dir.path().join("Resources/Views/edit.leaf"),
            "old\n",
        )?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
*** Begin Patch
Resources/Views/edit.leaf
@@
-old
+new
*** End Patch
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(
            fs::read_to_string(dir.path().join("Resources/Views/edit.leaf"))?,
            "new\n"
        );
        Ok(())
    }

    #[test]
    fn unescapes_json_escaped_quotes_in_patch_lines() -> Result<()> {
        let dir = tempdir()?;
        fs::write(
            dir.path().join("demo.swift"),
            "let name = \"Old\"\n",
        )?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
*** Begin Patch
*** Update File: demo.swift
@@
-let name = \"Old\"
+let name = \\\"Ink\\\"
*** End Patch
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(fs::read_to_string(dir.path().join("demo.swift"))?, "let name = \"Ink\"\n");
        Ok(())
    }

    #[test]
    fn ignores_embedded_diff_headers_inside_codex_update_section() -> Result<()> {
        let dir = tempdir()?;
        fs::write(dir.path().join("Package.swift"), "let a = 1\n")?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "\
*** Begin Patch
*** Update File: Package.swift
--- a/Package.swift
+++ b/Package.swift
@@
-let a = 1
+let a = 2
*** End Patch
"
                .to_string(),
                approved: Some(true),
            },
        )?;

        assert!(result.valid, "{}", result.check_stderr);
        assert!(result.applied, "{}", result.apply_stderr);
        assert_eq!(fs::read_to_string(dir.path().join("Package.swift"))?, "let a = 2\n");
        Ok(())
    }

    #[test]
    fn rejects_non_patch_payload_with_clear_message() -> Result<()> {
        let dir = tempdir()?;
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .status()?;
        assert!(status.success());

        let result = run(
            dir.path(),
            ApplyPatchArgs {
                patch: "Please update the file with the following change.".to_string(),
                approved: Some(true),
            },
        )?;

        assert!(!result.valid);
        assert!(result.check_stderr.contains("expected a patch body"));
        Ok(())
    }
}
