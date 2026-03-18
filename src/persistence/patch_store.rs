use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchRecord {
    pub ts_ms: u128,
    pub repo_root: String,
    pub session_log_path: Option<String>,
    pub patch: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UndoResult {
    pub undone: bool,
    pub message: String,
}

pub struct PatchStore {
    path: PathBuf,
}

impl PatchStore {
    pub fn default() -> Result<Self> {
        let root = default_root_dir()?;
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create patch store dir {}", root.display()))?;
        Ok(Self {
            path: root.join("patches.jsonl"),
        })
    }

    pub fn append(
        &self,
        repo_root: &Path,
        patch: &str,
        session_log_path: Option<&Path>,
    ) -> Result<()> {
        let record = PatchRecord {
            ts_ms: now_ms(),
            repo_root: normalize(repo_root).to_string_lossy().to_string(),
            session_log_path: session_log_path.map(|p| p.to_string_lossy().to_string()),
            patch: patch.to_string(),
        };
        let line = serde_json::to_string(&record).context("failed to serialize patch record")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open patch store {}", self.path.display()))?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        Ok(())
    }

    pub fn undo_last_for_repo(&self, repo_root: &Path) -> Result<UndoResult> {
        let records = self.read_all()?;
        let normalized_repo = normalize(repo_root).to_string_lossy().to_string();
        let Some(record) = records
            .iter()
            .rev()
            .find(|r| normalize(Path::new(&r.repo_root)).to_string_lossy() == normalized_repo)
        else {
            return Ok(UndoResult {
                undone: false,
                message: "no patch history found for current repository".to_string(),
            });
        };

        let (ok, stderr) = git_apply_reverse(repo_root, &record.patch)?;
        if !ok {
            return Ok(UndoResult {
                undone: false,
                message: format!("failed to reverse patch: {}", stderr.trim()),
            });
        }
        Ok(UndoResult {
            undone: true,
            message: "reversed most recent applied patch for repository".to_string(),
        })
    }

    fn read_all(&self) -> Result<Vec<PatchRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .with_context(|| format!("failed to read patch store {}", self.path.display()))?;
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(record) = serde_json::from_str::<PatchRecord>(&line) {
                out.push(record);
            }
        }
        Ok(out)
    }
}

fn git_apply_reverse(repo_root: &Path, patch: &str) -> Result<(bool, String)> {
    let mut cmd = Command::new("git");
    cmd.arg("apply")
        .arg("-R")
        .current_dir(repo_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("failed to spawn git apply -R")?;
    {
        let stdin = child.stdin.as_mut().context("failed to open stdin")?;
        stdin
            .write_all(patch.as_bytes())
            .context("failed to write patch to stdin")?;
    }
    let output = child
        .wait_with_output()
        .context("failed to wait for git apply -R")?;
    let ok = output.status.success();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok((ok, stderr))
}

fn default_root_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".local/share/grok-agent"))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn normalize(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
