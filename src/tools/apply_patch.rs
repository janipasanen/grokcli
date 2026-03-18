use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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

    let (check_ok, check_stderr) = run_git_apply(repo_root, &normalized_patch, true)?;
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

    let (apply_ok, apply_stderr) = run_git_apply(repo_root, &normalized_patch, false)?;
    Ok(ApplyPatchResult {
        valid: true,
        applied: apply_ok,
        approval_required: false,
        check_stderr,
        apply_stderr,
    })
}

fn run_git_apply(repo_root: &Path, patch: &str, check_only: bool) -> Result<(bool, String)> {
    let mut cmd = Command::new("git");
    cmd.arg("apply");
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

fn normalize_patch(patch: &str) -> std::result::Result<String, String> {
    let patch = sanitize_patch_input(patch);
    if patch.lines().any(|line| line.starts_with("*** Begin Patch")) {
        return codex_patch_to_unified_diff(&patch);
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
        if line.is_empty() {
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File: ") {
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
                section.push(normalize_hunk_line(next));
                let _ = lines.next();
            }
            if section.is_empty() {
                return Err(format!(
                    "invalid Codex patch format for `{path}`: empty update section"
                ));
            }
            let section = normalize_codex_hunks_or_wrap(&section);
            let _ = writeln!(unified, "diff --git a/{path} b/{new_path}");
            let _ = writeln!(unified, "--- a/{path}");
            let _ = writeln!(unified, "+++ b/{new_path}");
            for hunk_line in section {
                let _ = writeln!(unified, "{hunk_line}");
            }
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

        return Err(format!("invalid Codex patch format line: `{line}`"));
    }

    if unified.trim().is_empty() {
        return Err("invalid Codex patch format: no file operations found".to_string());
    }

    Ok(unified)
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

fn normalize_unified_diff_hunks(patch: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_hunk = false;
    let mut old_start = 1usize;
    let mut new_start = 1usize;
    let mut hunk_start_idx: Option<usize> = None;
    let mut old_count = 0usize;
    let mut new_count = 0usize;

    let flush_hunk_header = |out: &mut Vec<String>,
                             hunk_start_idx: Option<usize>,
                             old_start: usize,
                             old_count: usize,
                             new_start: usize,
                             new_count: usize| {
        if let Some(idx) = hunk_start_idx
            && out.get(idx).map(|line| line.trim()) == Some("@@")
        {
            out[idx] = format!(
                "@@ -{} +{} @@",
                hunk_range(old_start, old_count),
                hunk_range(new_start, new_count)
            );
        }
    };

    for raw_line in patch.lines() {
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
                old_count,
                new_start,
                new_count,
            );
            old_start += old_count;
            new_start += new_count;
            old_count = 0;
            new_count = 0;
            hunk_start_idx = None;
            if is_header {
                in_hunk = false;
            }
        }

        if raw_line.starts_with("@@") {
            in_hunk = true;
            let line = raw_line.trim().to_string();
            if line == "@@" {
                hunk_start_idx = Some(out.len());
            }
            out.push(line);
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
    if line.starts_with("\\ No newline at end of file") {
        return line.to_string();
    }
    match line.chars().next() {
        Some(' ') | Some('+') | Some('-') | Some('@') => line.to_string(),
        _ => format!(" {line}"),
    }
}

fn normalize_add_file_line(line: &str) -> String {
    if line.starts_with("\\ No newline at end of file") {
        return line.to_string();
    }
    match line.chars().next() {
        Some('+') => line.to_string(),
        Some(' ') => format!("+{}", &line[1..]),
        Some('-') => format!("+{}", &line[1..]),
        _ => format!("+{line}"),
    }
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
}
