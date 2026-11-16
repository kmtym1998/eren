use std::path::{Path, PathBuf};
use std::process::Command;

/// git worktree 1件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// HEAD のコミットハッシュ（bare の場合は None のことがある）。
    pub head: Option<String>,
    /// ブランチ名（`refs/heads/` を除いた短い名前）。detached / bare では None。
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// `git worktree list --porcelain` の出力をパースする。
pub fn parse_worktree_list(output: &str) -> Vec<Worktree> {
    fn finish(current: &mut Option<Worktree>, result: &mut Vec<Worktree>) {
        if let Some(mut worktree) = current.take() {
            if worktree.bare || worktree.detached {
                worktree.branch = None;
            }
            result.push(worktree);
        }
    }

    let mut result = Vec::new();
    let mut current: Option<Worktree> = None;
    for line in output.lines() {
        if line.is_empty() {
            finish(&mut current, &mut result);
        } else if let Some(path) = line.strip_prefix("worktree ") {
            finish(&mut current, &mut result);
            if !path.is_empty() {
                current = Some(Worktree {
                    path: PathBuf::from(path),
                    head: None,
                    branch: None,
                    bare: false,
                    detached: false,
                    locked: false,
                    prunable: false,
                });
            }
        } else if let Some(worktree) = current.as_mut() {
            if let Some(head) = line.strip_prefix("HEAD ") {
                if !head.is_empty() {
                    worktree.head = Some(head.to_owned());
                }
            } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
                if !branch.is_empty() {
                    worktree.branch = Some(branch.to_owned());
                }
            } else if line == "detached" {
                worktree.detached = true;
            } else if line == "bare" {
                worktree.bare = true;
            } else if line == "locked" || line.starts_with("locked ") {
                worktree.locked = true;
            } else if line == "prunable" || line.starts_with("prunable ") {
                worktree.prunable = true;
            }
        }
    }
    finish(&mut current, &mut result);
    result
}

/// 指定フォルダで `git worktree list --porcelain` を実行して一覧を返す。
/// git リポジトリでない場合や git が失敗した場合は Err(理由の文字列)。
pub fn list_worktrees(root: &Path) -> Result<Vec<Worktree>, String> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("git を実行できなかった: {error}"))?;

    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git worktree list に失敗した: {}", reason.trim()));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("git worktree list の出力が不正: {error}"))?;
    Ok(parse_worktree_list(&stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_branch_worktrees_with_spaces() {
        let worktrees = parse_worktree_list(
            "worktree /repo/main\nHEAD abc123\nbranch refs/heads/main\n\n\
             worktree /repo/feature worktree\nHEAD def456\nbranch refs/heads/feature/topic\n\n",
        );
        assert_eq!(
            worktrees,
            vec![
                Worktree {
                    path: PathBuf::from("/repo/main"),
                    head: Some("abc123".into()),
                    branch: Some("main".into()),
                    bare: false,
                    detached: false,
                    locked: false,
                    prunable: false,
                },
                Worktree {
                    path: PathBuf::from("/repo/feature worktree"),
                    head: Some("def456".into()),
                    branch: Some("feature/topic".into()),
                    bare: false,
                    detached: false,
                    locked: false,
                    prunable: false,
                },
            ]
        );
    }

    #[test]
    fn detached_worktree_without_final_newline() {
        let worktrees = parse_worktree_list("worktree /repo/detached\nHEAD abc123\ndetached");
        assert_eq!(worktrees.len(), 1);
        assert!(worktrees[0].detached);
        assert_eq!(worktrees[0].head.as_deref(), Some("abc123"));
        assert_eq!(worktrees[0].branch, None);
        assert!(!worktrees[0].bare);
    }

    #[test]
    fn bare_worktree() {
        let worktrees = parse_worktree_list("worktree /repo/bare.git\nbare\n\n");
        assert_eq!(worktrees.len(), 1);
        assert!(worktrees[0].bare);
        assert_eq!(worktrees[0].head, None);
        assert_eq!(worktrees[0].branch, None);
        assert!(!worktrees[0].detached);
    }

    #[test]
    fn locked_and_prunable_with_and_without_reasons() {
        for flags in ["locked\nprunable", "locked in use\nprunable gitdir missing"] {
            let worktrees = parse_worktree_list(&format!("worktree /repo/task\n{flags}\n"));
            assert_eq!(worktrees.len(), 1);
            assert!(worktrees[0].locked);
            assert!(worktrees[0].prunable);
        }
    }

    #[test]
    fn empty_output() {
        assert!(parse_worktree_list("").is_empty());
        assert!(parse_worktree_list("\n\n").is_empty());
    }

    #[test]
    fn ignores_unknown_lines_and_blocks_without_paths() {
        let worktrees = parse_worktree_list(
            "HEAD orphan\nunknown\n\nworktree \nlocked\n\n\
             worktree /repo/task\nunknown value\nlockedness\nprunables\n\n\n",
        );
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].path, PathBuf::from("/repo/task"));
        assert_eq!(worktrees[0].head, None);
        assert!(!worktrees[0].locked);
        assert!(!worktrees[0].prunable);
    }

    #[test]
    fn handles_crlf_and_missing_block_separator() {
        let worktrees = parse_worktree_list(
            "worktree /repo/main\r\nbranch refs/heads/main\r\nworktree /repo/other\r\ndetached\r\n",
        );
        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
        assert_eq!(worktrees[1].path, PathBuf::from("/repo/other"));
        assert!(worktrees[1].detached);
    }

    #[test]
    fn detached_and_bare_never_have_branches() {
        for flag in ["detached", "bare"] {
            for fields in [
                format!("branch refs/heads/main\n{flag}"),
                format!("{flag}\nbranch refs/heads/main"),
            ] {
                let worktrees = parse_worktree_list(&format!("worktree /repo/task\n{fields}"));
                assert_eq!(worktrees[0].branch, None);
            }
        }
    }

    #[test]
    fn reports_invalid_working_directory() {
        // このソースファイルはディレクトリではないため、git の実行に失敗する。
        assert!(list_worktrees(Path::new(file!())).is_err());
    }
}
