use crate::persistence::patch_store::{PatchStore, UndoResult};
use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct UndoLastPatchArgs {
    pub approved: Option<bool>,
}

pub fn run(repo_root: &Path, args: UndoLastPatchArgs) -> Result<UndoResult> {
    if !args.approved.unwrap_or(false) {
        return Ok(UndoResult {
            undone: false,
            message: "undo_last_patch requires approval".to_string(),
        });
    }
    let store = PatchStore::default()?;
    store.undo_last_for_repo(repo_root)
}
