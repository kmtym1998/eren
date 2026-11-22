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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background_task::BackgroundTask;
    use std::{
        path::Path,
        process::Command,
        time::{Duration, Instant},
    };

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn read(root: &Path) -> GitSnapshot {
        let mut task = BackgroundTask::default();
        let root = root.to_path_buf();
        task.submit(move |cancel| GitSnapshot::read(root, cancel));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = task.poll() {
                return result.unwrap();
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    #[test]
    fn worker_snapshot_keeps_unborn_and_detached_head_labels() {
        let root = std::env::temp_dir().join(format!("eren-git-snapshot-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "fixture"]);
        assert_eq!(read(&root).branch.as_deref(), Some("fixture"));
        git(&root, &["config", "user.email", "fixture@example.invalid"]);
        git(&root, &["config", "user.name", "Fixture"]);
        std::fs::write(root.join("file"), "fixture").unwrap();
        git(&root, &["add", "file"]);
        git(&root, &["commit", "-qm", "fixture"]);
        git(&root, &["checkout", "-q", "--detach"]);
        let snapshot = read(&root);
        assert!(snapshot.branch.unwrap().starts_with("detached HEAD ("));
        assert!(snapshot.changes.unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
