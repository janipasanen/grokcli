use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct ListDirectoryArgs {
    pub path: Option<String>,
    pub include_hidden: Option<bool>,
    pub max_entries: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListDirectoryResult {
    pub path: String,
    pub entries: Vec<String>,
    pub was_truncated: bool,
}

pub fn run(repo_root: &Path, args: ListDirectoryArgs) -> Result<ListDirectoryResult> {
    let rel = args.path.unwrap_or_else(|| ".".to_string());
    let target = repo_root.join(&rel);
    if !target.starts_with(repo_root) {
        bail!("list_directory path escapes repo root");
    }
    let include_hidden = args.include_hidden.unwrap_or(false);
    let max_entries = args.max_entries.unwrap_or(500);

    let mut entries = Vec::new();
    for entry_result in fs::read_dir(&target)? {
        let entry = entry_result?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !include_hidden && name.starts_with('.') {
            continue;
        }
        entries.push(name);
        if entries.len() >= max_entries {
            break;
        }
    }
    entries.sort();

    Ok(ListDirectoryResult {
        path: rel,
        was_truncated: entries.len() == max_entries,
        entries,
    })
}
