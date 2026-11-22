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

/// `git worktree list --porcelain -z` の出力をパースする。
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
    for line in output.split('\0') {
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

/// 指定フォルダで `git worktree list --porcelain -z` を実行して一覧を返す。
/// git リポジトリでない場合や git が失敗した場合は Err(理由の文字列)。
pub fn list_worktrees(root: &Path) -> Result<Vec<Worktree>, String> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain", "-z"])
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
            "worktree /repo/main\0HEAD abc123\0branch refs/heads/main\0\0\
             worktree /repo/feature worktree\0HEAD def456\0branch refs/heads/feature/topic\0\0",
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
    fn detached_worktree_without_final_delimiter() {
        let worktrees = parse_worktree_list("worktree /repo/detached\0HEAD abc123\0detached");
        assert_eq!(worktrees.len(), 1);
        assert!(worktrees[0].detached);
        assert_eq!(worktrees[0].head.as_deref(), Some("abc123"));
        assert_eq!(worktrees[0].branch, None);
        assert!(!worktrees[0].bare);
    }

    #[test]
    fn bare_worktree() {
        let worktrees = parse_worktree_list("worktree /repo/bare.git\0bare\0\0");
        assert_eq!(worktrees.len(), 1);
        assert!(worktrees[0].bare);
        assert_eq!(worktrees[0].head, None);
        assert_eq!(worktrees[0].branch, None);
        assert!(!worktrees[0].detached);
    }

    #[test]
    fn locked_and_prunable_with_and_without_reasons() {
        for flags in ["locked\0prunable", "locked in use\0prunable gitdir missing"] {
            let worktrees = parse_worktree_list(&format!("worktree /repo/task\0{flags}\0"));
            assert_eq!(worktrees.len(), 1);
            assert!(worktrees[0].locked);
            assert!(worktrees[0].prunable);
        }
    }

    #[test]
    fn empty_output() {
        assert!(parse_worktree_list("").is_empty());
        assert!(parse_worktree_list("\0\0").is_empty());
    }

    #[test]
    fn ignores_unknown_fields_and_records_without_paths() {
        let worktrees = parse_worktree_list(
            "HEAD orphan\0unknown\0\0worktree \0locked\0\0\
             worktree /repo/task\0unknown value\0lockedness\0prunables\0\0\0",
        );
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].path, PathBuf::from("/repo/task"));
        assert_eq!(worktrees[0].head, None);
        assert!(!worktrees[0].locked);
        assert!(!worktrees[0].prunable);
    }

    #[test]
    fn handles_missing_record_separator() {
        let worktrees = parse_worktree_list(
            "worktree /repo/main\0branch refs/heads/main\0worktree /repo/other\0detached\0",
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
                format!("branch refs/heads/main\0{flag}"),
                format!("{flag}\0branch refs/heads/main"),
            ] {
                let worktrees = parse_worktree_list(&format!("worktree /repo/task\0{fields}"));
                assert_eq!(worktrees[0].branch, None);
            }
        }
    }

    #[test]
    fn whitespace_inside_paths_and_reasons_is_not_a_field_separator() {
        let path = "/repo/日本語 space\ttab\nnewline\r";
        let output = format!(
            "worktree {path}\0HEAD abc\0branch refs/heads/topic\0locked reason\nwith newline\0\0"
        );
        let parsed = parse_worktree_list(&output);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].path, PathBuf::from(path));
        assert_eq!(parsed[0].branch.as_deref(), Some("topic"));
        assert!(parsed[0].locked);
    }

    #[test]
    fn real_git_worktrees_preserve_spaces_japanese_tabs_and_newlines() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "eren-worktree-paths-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let repository = directory.join("main repo");
        std::fs::create_dir(&repository).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&repository)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q", "--template="]);
        git(&["commit", "--allow-empty", "-qm", "fixture"]);
        let mut expected = vec![repository.canonicalize().unwrap()];
        for (index, name) in ["space path", "日本語", "tab\tpath", "newline\npath"]
            .into_iter()
            .enumerate()
        {
            let path = directory.join(name);
            git(&[
                "worktree",
                "add",
                "-qb",
                &format!("fixture-{index}"),
                path.to_str().unwrap(),
            ]);
            expected.push(path.canonicalize().unwrap());
        }
        for root in [&repository, &expected[4]] {
            let parsed = list_worktrees(root).unwrap();
            assert_eq!(parsed.len(), expected.len());
            let mut actual: Vec<_> = parsed.into_iter().map(|entry| entry.path).collect();
            actual.sort();
            let mut expected = expected.clone();
            expected.sort();
            assert_eq!(actual, expected);
            assert!(actual.iter().all(|path| path.is_dir()));
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reports_invalid_working_directory() {
        // このソースファイルはディレクトリではないため、git の実行に失敗する。
        assert!(list_worktrees(Path::new(file!())).is_err());
    }
}
