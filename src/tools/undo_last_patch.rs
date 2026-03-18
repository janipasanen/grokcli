use crate::persistence::patch_store::{PatchStore, UndoResult};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct UndoLastPatchArgs {
    pub approved: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoLastPatchResult {
    pub approval_required: bool,
    pub undone: bool,
    pub message: String,
}

pub fn run(repo_root: &Path, args: UndoLastPatchArgs) -> Result<UndoLastPatchResult> {
    if !args.approved.unwrap_or(false) {
        return Ok(UndoLastPatchResult {
            approval_required: true,
            undone: false,
            message: "undo_last_patch requires approval".to_string(),
        });
    }
    let store = PatchStore::default()?;
    let UndoResult { undone, message } = store.undo_last_for_repo(repo_root)?;
    Ok(UndoLastPatchResult {
        approval_required: false,
        undone,
        message,
    })
}
