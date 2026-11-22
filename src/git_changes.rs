//! Git paths are always relative to the repository root, independently of the project folder.
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub original_path: Option<String>,
    pub index_status: char,
    pub worktree_status: char,
}

fn run(root: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("git を実行できなかった: {error}"))
}

fn failure(operation: &str, output: &Output) -> String {
    let reason = String::from_utf8_lossy(&output.stderr);
    format!(
        "git {operation} に失敗した（終了コード {:?}）: {}",
        output.status.code(),
        reason.trim()
    )
}

pub fn repository_root(project: &Path) -> Result<PathBuf, String> {
    let output = run(project, &["rev-parse", "--show-toplevel"])?;
    if !output.status.success() {
        return Err(failure("rev-parse", &output));
    }
    let path = String::from_utf8(output.stdout)
        .map_err(|_| "Git ルートのパスが UTF-8 ではない".to_string())?;
    let path = path.strip_suffix('\n').unwrap_or(&path);
    Ok(PathBuf::from(path))
}

pub fn parse_porcelain(bytes: &[u8]) -> Result<Vec<ChangedFile>, String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "Git status のパスが UTF-8 ではない".to_string())?;
    if !text.is_empty() && !text.ends_with('\0') {
        return Err("Git status のレコードが途中で切れている".into());
    }
    let mut entries = text.split_terminator('\0');
    let mut files = Vec::new();
    while let Some(entry) = entries.next() {
        let mut chars = entry.chars();
        let (Some(index_status), Some(worktree_status), Some(' ')) =
            (chars.next(), chars.next(), chars.next())
        else {
            return Err("Git status のレコードが不正".into());
        };
        let path: String = chars.collect();
        if path.is_empty() {
            return Err("Git status のパスが空".into());
        }
        let original_path =
            if matches!(index_status, 'R' | 'C') || matches!(worktree_status, 'R' | 'C') {
                Some(
                    entries
                        .next()
                        .filter(|path| !path.is_empty())
                        .ok_or("Git status に移動元のパスがない")?
                        .to_string(),
                )
            } else {
                None
            };
        files.push(ChangedFile {
            path,
            original_path,
            index_status,
            worktree_status,
        });
    }
    Ok(files)
}

pub fn status(project: &Path) -> Result<Vec<ChangedFile>, String> {
    let root = repository_root(project)?;
    let output = run(
        &root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--find-renames",
        ],
    )?;
    if !output.status.success() {
        return Err(failure("status", &output));
    }
    parse_porcelain(&output.stdout)
}

fn comparison_base(root: &Path) -> Result<String, String> {
    let head = run(root, &["rev-parse", "--verify", "--quiet", "HEAD"])?;
    if head.status.success() {
        return Ok("HEAD".into());
    }
    let symbolic = run(root, &["symbolic-ref", "--quiet", "HEAD"])?;
    if head.status.code() != Some(1) || !symbolic.status.success() {
        return Err(failure("rev-parse HEAD", &head));
    }
    let reference =
        String::from_utf8(symbolic.stdout).map_err(|_| "Git HEAD の出力が不正".to_string())?;
    let reference = reference.trim_end_matches('\n');
    let exists = run(root, &["show-ref", "--verify", "--quiet", reference])?;
    if exists.status.code() != Some(1) {
        return Err(failure("show-ref HEAD", &exists));
    }
    // Git recognizes the empty tree for either SHA-1 or SHA-256 without writing an object.
    let empty = run(root, &["hash-object", "-t", "tree", "--stdin"])?;
    if !empty.status.success() {
        return Err(failure("hash-object", &empty));
    }
    String::from_utf8(empty.stdout)
        .map(|hash| hash.trim_end_matches('\n').to_string())
        .map_err(|_| "Git の empty tree が不正".into())
}

pub fn diff(project: &Path, file: &ChangedFile) -> Result<String, String> {
    let root = repository_root(project)?;
    let untracked = file.index_status == '?' && file.worktree_status == '?';
    let output = if untracked {
        run(
            &root,
            &[
                "--literal-pathspecs",
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--no-index",
                "--",
                "/dev/null",
                &file.path,
            ],
        )?
    } else {
        let base = comparison_base(&root)?;
        let mut args = vec![
            "--literal-pathspecs",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--find-renames",
            "--find-copies-harder",
            &base,
            "--",
        ];
        if let Some(original) = &file.original_path {
            args.push(original);
        }
        args.push(&file.path);
        run(&root, &args)?
    };
    // --no-index also returns 1 for e.g. a disappeared input, with an error on stderr.
    let accepted = output.status.success()
        || (untracked && output.status.code() == Some(1) && output.stderr.is_empty());
    if !accepted {
        return Err(failure("diff", &output));
    }
    String::from_utf8(output.stdout).map_err(|_| "Git diff の出力が UTF-8 ではない".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Repository {
        directory: PathBuf,
        root: PathBuf,
    }
    impl Repository {
        fn new() -> Self {
            Self::with_format("sha1")
        }
        fn with_format(format: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!(
                "eren-git-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let root = directory.join("repo 日本語 space");
            std::fs::create_dir_all(&root).unwrap();
            let repo = Self { directory, root };
            repo.git(&[
                "init",
                "-q",
                "--template=",
                &format!("--object-format={format}"),
            ]);
            repo.git(&["config", "user.name", "Fixture"]);
            repo.git(&["config", "user.email", "fixture@example.invalid"]);
            repo.git(&["config", "commit.gpgsign", "false"]);
            repo.git(&["config", "core.quotePath", "false"]);
            repo
        }
        fn git(&self, args: &[&str]) -> Output {
            let output = Command::new("git")
                .args(["-c", "core.hooksPath=/dev/null"])
                .args(args)
                .current_dir(&self.root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output
        }
        fn write(&self, path: &str, text: &str) {
            let path = self.root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        fn commit(&self) {
            self.git(&["add", "."]);
            self.git(&["commit", "-qm", "fixture"]);
        }
    }
    impl Drop for Repository {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn subfolder_and_linked_worktree_use_repository_relative_paths() {
        let repo = Repository::new();
        let path = "sub folder/日本語 file.txt";
        repo.write(path, "before\n");
        repo.commit();
        repo.write(path, "after\n");
        let project = repo.root.join("sub folder");
        assert_eq!(
            repository_root(&project).unwrap(),
            repo.root.canonicalize().unwrap()
        );
        let files = status(&project).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, path);
        let patch = diff(&project, &files[0]).unwrap();
        assert!(patch.contains("-before\n+after"), "{patch}");
        let linked = repo.directory.join("linked 日本語 space");
        repo.git(&[
            "worktree",
            "add",
            "-qb",
            "fixture-linked",
            linked.to_str().unwrap(),
        ]);
        std::fs::write(linked.join(path), "linked edit\n").unwrap();
        let sub = linked.join("sub folder");
        let files = status(&sub).unwrap();
        assert_eq!(
            repository_root(&sub).unwrap(),
            linked.canonicalize().unwrap()
        );
        assert!(diff(&sub, &files[0]).unwrap().contains("+linked edit"));
    }

    #[test]
    fn unborn_head_supports_staged_and_working_tree_content() {
        let repo = Repository::new();
        repo.write("new.txt", "staged\n");
        repo.git(&["add", "new.txt"]);
        let files = status(&repo.root).unwrap();
        assert!(diff(&repo.root, &files[0]).unwrap().contains("+staged"));
        repo.write("new.txt", "working\n");
        assert!(diff(&repo.root, &files[0]).unwrap().contains("+working"));
        assert!(!repo
            .root
            .join(".git/refs/heads")
            .read_dir()
            .unwrap()
            .any(|entry| entry.is_ok()));
    }

    #[test]
    fn unborn_sha256_repository_uses_its_own_empty_tree() {
        let repo = Repository::with_format("sha256");
        repo.write("new.txt", "sha256 staged\n");
        repo.git(&["add", "."]);
        let files = status(&repo.root).unwrap();
        assert!(diff(&repo.root, &files[0])
            .unwrap()
            .contains("+sha256 staged"));
    }

    #[test]
    fn untracked_binary_unchanged_and_git_errors_are_distinct() {
        let repo = Repository::new();
        repo.write("tracked.txt", "same\n");
        repo.commit();
        let unchanged = ChangedFile {
            path: "tracked.txt".into(),
            ..Default::default()
        };
        assert_eq!(diff(&repo.root, &unchanged).unwrap(), "");
        repo.write("nested/new.txt", "new\n");
        let file = status(&repo.root)
            .unwrap()
            .into_iter()
            .find(|file| file.path == "nested/new.txt")
            .unwrap();
        assert_eq!((file.index_status, file.worktree_status), ('?', '?'));
        assert!(diff(&repo.root, &file).unwrap().contains("+new")); // --no-index returns 1.
        std::fs::write(repo.root.join("binary"), [0, 255, 1]).unwrap();
        let binary = status(&repo.root)
            .unwrap()
            .into_iter()
            .find(|file| file.path == "binary")
            .unwrap();
        assert!(diff(&repo.root, &binary).unwrap().contains("Binary files"));
        std::fs::remove_file(repo.root.join(&file.path)).unwrap();
        let error = diff(&repo.root, &file).unwrap_err();
        assert!(error.contains("git diff に失敗"), "{error}");
        assert!(status(&repo.directory).unwrap_err().contains("rev-parse"));
        std::fs::write(repo.root.join(".git/index"), "corrupted index").unwrap();
        assert!(status(&repo.root)
            .unwrap_err()
            .contains("git status に失敗"));
        let error = diff(&repo.root, &unchanged).unwrap_err();
        assert!(error.contains("index"), "{error}");
    }

    #[test]
    fn rename_keeps_both_paths_and_changed_content() {
        let repo = Repository::new();
        let old = "sub folder/元 file.txt";
        let new = "sub folder/先 file.txt";
        let text = (0..20)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        repo.write(old, &text);
        repo.commit();
        repo.git(&["mv", old, new]);
        let files = status(&repo.root).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].original_path.as_deref(), Some(old));
        assert_eq!(files[0].path, new);
        let patch = diff(&repo.root.join("sub folder"), &files[0]).unwrap();
        assert!(patch.contains("similarity index 100%"), "{patch}");
        assert!(patch.contains(&format!("rename from {old}")), "{patch}");
        assert!(patch.contains(&format!("rename to {new}")), "{patch}");
        repo.write(new, &text.replace("line 10\n", "edited ten\n"));
        let patch = diff(&repo.root, &status(&repo.root).unwrap()[0]).unwrap();
        assert!(patch.contains("rename from"));
        assert!(patch.contains("-line 10\n+edited ten"), "{patch}");
    }

    #[test]
    fn porcelain_preserves_rename_copy_and_whitespace_paths() {
        let files =
            parse_porcelain(b"R  new name\0old name\0C  copy\0source\0 M tab\tand\nnewline\0")
                .unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].original_path.as_deref(), Some("old name"));
        assert_eq!(files[1].original_path.as_deref(), Some("source"));
        assert_eq!(files[2].path, "tab\tand\nnewline");
        assert!(parse_porcelain(b"R  new\0").is_err());
        assert!(parse_porcelain(b" M truncated").is_err());
        assert!(parse_porcelain(b"?? invalid\xff\0").is_err());
    }

    #[test]
    fn copied_file_diff_includes_source_and_paths_are_literal() {
        let repo = Repository::new();
        repo.write("source", "one\ntwo\nthree\n");
        repo.commit();
        repo.write("copy 日本語", "one\ntwo\nthree\n");
        repo.write("source", "changed one\ntwo\nthree\n");
        repo.git(&["add", "."]);
        repo.git(&["config", "status.renames", "copies"]);
        let copy = status(&repo.root)
            .unwrap()
            .into_iter()
            .find(|file| file.path == "copy 日本語")
            .unwrap();
        assert_eq!(copy.index_status, 'C');
        assert_eq!(copy.original_path.as_deref(), Some("source"));
        let patch = diff(&repo.root, &copy).unwrap();
        assert!(patch.contains("copy from source"), "{patch}");
        repo.write(":literal[1]", "before\n");
        repo.commit();
        repo.write(":literal[1]", "after\n");
        let file = status(&repo.root)
            .unwrap()
            .into_iter()
            .find(|file| file.path == ":literal[1]")
            .unwrap();
        assert!(diff(&repo.root, &file).unwrap().contains("+after"));
    }
}
