use crate::{
    background_task::Cancellation,
    git_changes::{self, ChangedFile},
    worktrees::{self, Worktree},
};
use std::path::PathBuf;

pub struct GitSnapshot {
    pub project: PathBuf,
    pub changes: Result<Vec<ChangedFile>, String>,
    pub worktrees: Result<Vec<Worktree>, String>,
    pub branch: Option<String>,
}
impl GitSnapshot {
    pub fn read(project: PathBuf, cancel: Cancellation) -> Self {
        let changes = git_changes::status(&project);
        let worktrees = if cancel.is_cancelled() {
            Ok(Vec::new())
        } else {
            worktrees::list_worktrees(&project)
        };
        let branch = if cancel.is_cancelled() {
            None
        } else {
            git_changes::branch(&project).ok()
        };
        Self {
            project,
            changes,
            worktrees,
            branch,
        }
    }
}
pub struct DiffSnapshot {
    pub project: PathBuf,
    pub path: String,
    pub result: Result<String, String>,
}
