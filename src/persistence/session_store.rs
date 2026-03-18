use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        let root = sessions_dir()?;
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create session dir {}", root.display()))?;
        let session_id = now_ms();
        let path = root.join(format!("{session_id}.jsonl"));
        Ok(Self { path })
    }

    pub fn open_existing(path: PathBuf) -> Result<Self> {
        if !path.exists() {
            anyhow::bail!("session file does not exist: {}", path.display());
        }
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

    pub fn read_events(&self) -> Result<Vec<SessionEvent>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .with_context(|| format!("failed to open session file {}", self.path.display()))?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<SessionEvent>(&line) {
                events.push(event);
            }
        }
        Ok(events)
    }

    pub fn last_provider_response_id(&self) -> Result<Option<String>> {
        let events = self.read_events()?;
        for event in events.iter().rev() {
            if event.event_type != "provider_response" {
                continue;
            }
            if let Some(id) = event.payload.get("id").and_then(serde_json::Value::as_str) {
                return Ok(Some(id.to_string()));
            }
        }
        Ok(None)
    }

    pub fn prompt_payload(prompt: &str) -> serde_json::Value {
        json!({ "prompt": prompt })
    }

    pub fn assistant_payload(output: &str) -> serde_json::Value {
        json!({ "output": output })
    }
}

pub fn sessions_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".local/share/grok-agent/sessions"))
}

pub fn resolve_session_path(spec: &str) -> Result<PathBuf> {
    if spec.contains('/') {
        return Ok(PathBuf::from(spec));
    }
    let file = if spec.ends_with(".jsonl") {
        spec.to_string()
    } else {
        format!("{spec}.jsonl")
    };
    Ok(sessions_dir()?.join(file))
}

pub fn latest_session_path() -> Result<Option<PathBuf>> {
    let dir = sessions_dir()?;
    if !dir.exists() {
        return Ok(None);
    }
    let mut best: Option<(u128, PathBuf)> = None;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let key = stem.parse::<u128>().unwrap_or(0);
        match &best {
            Some((current, _)) if *current >= key => {}
            _ => best = Some((key, path)),
        }
    }
    Ok(best.map(|(_, path)| path))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
