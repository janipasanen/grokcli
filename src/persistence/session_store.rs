use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct SessionEvent {
    pub ts_ms: u128,
    pub event_type: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    pub fn for_new_session() -> Result<Self> {
        let root = default_sessions_dir()?;
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create session dir {}", root.display()))?;
        let session_id = now_ms();
        let path = root.join(format!("{session_id}.jsonl"));
        Ok(Self { path })
    }

    pub fn append(&self, event_type: &str, payload: serde_json::Value) -> Result<()> {
        let event = SessionEvent {
            ts_ms: now_ms(),
            event_type: event_type.to_string(),
            payload,
        };
        let line = serde_json::to_string(&event).context("failed to serialize session event")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open session file {}", self.path.display()))?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        Ok(())
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn prompt_payload(prompt: &str) -> serde_json::Value {
        json!({ "prompt": prompt })
    }

    pub fn assistant_payload(output: &str) -> serde_json::Value {
        json!({ "output": output })
    }
}

fn default_sessions_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".local/share/grok-agent/sessions"))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
