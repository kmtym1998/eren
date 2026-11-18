#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    /// `diff --git` / `---` / `+++` などのファイルヘッダ。
    FileHeader,
    /// `@@ -a,b +c,d @@` ハンクヘッダ。
    HunkHeader,
    /// 先頭が空白の文脈行。
    Context,
    /// 先頭が `+` の追加行。
    Added,
    /// 先頭が `-` の削除行。
    Removed,
    /// 改行の有無などのメタ行。
    Meta,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// 末尾の改行を除く原文（先頭の記号を含む）。
    pub text: String,
    /// この行の末尾には改行がない（Git の直後のメタ行から取得）。
    pub no_newline: bool,
    /// 旧ファイルの行番号（Context / Removed のみ）。
    pub old_lineno: Option<usize>,
    /// 新ファイルの行番号（Context / Added のみ）。
    pub new_lineno: Option<usize>,
}

fn range(text: &str, prefix: char) -> Option<(usize, usize)> {
    let text = text.strip_prefix(prefix)?;
    let (start, count) = text.split_once(',').unwrap_or((text, "1"));
    Some((start.parse().ok()?, count.parse().ok()?))
}

fn hunk(text: &str) -> Option<((usize, usize), (usize, usize))> {
    let mut fields = text.strip_prefix("@@ ")?.split_whitespace();
    let old = range(fields.next()?, '-')?;
    let new = range(fields.next()?, '+')?;
    (fields.next()? == "@@").then_some((old, new))
}

/// unified diff テキストを構造化する。行番号は各ハンクから数える。
pub fn parse_unified_diff(text: &str) -> Vec<DiffLine> {
    use DiffLineKind::*;
    let mut result: Vec<DiffLine> = Vec::new();
    let (mut old, mut new) = (None, None);
    let (mut old_left, mut new_left) = (0usize, 0usize);
    for text in text.lines() {
        if text == "\\ No newline at end of file" {
            if let Some(previous) = result
                .last_mut()
                .filter(|line| matches!(line.kind, Context | Added | Removed))
            {
                previous.no_newline = true;
                continue;
            }
        }
        let mut line = DiffLine {
            kind: Meta,
            text: text.to_owned(),
            no_newline: false,
            old_lineno: None,
            new_lineno: None,
        };
        if text.starts_with("@@") {
            line.kind = HunkHeader;
            old = None;
            new = None;
            old_left = 0;
            new_left = 0;
            if let Some(((a, b), (c, d))) = hunk(text) {
                old = Some(a);
                new = Some(c);
                old_left = b;
                new_left = d;
            }
        } else if text.starts_with("diff ")
            || text.starts_with("index ")
            || ((old_left == 0 && new_left == 0)
                && (text.starts_with("---") || text.starts_with("+++")))
        {
            line.kind = FileHeader;
            old = None;
            new = None;
            old_left = 0;
            new_left = 0;
        } else {
            line.kind = match text.as_bytes().first() {
                Some(b' ') => Context,
                Some(b'+') => Added,
                Some(b'-') => Removed,
                _ => Meta,
            };
            if matches!(line.kind, Context | Removed) {
                line.old_lineno = old;
                old = old.and_then(|n| n.checked_add(1));
                old_left = old_left.saturating_sub(1);
            }
            if matches!(line.kind, Context | Added) {
                line.new_lineno = new;
                new = new.and_then(|n| n.checked_add(1));
                new_left = new_left.saturating_sub(1);
            }
        }
        result.push(line);
    }
    result
}

/// side-by-side（split）表示用の行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SplitRow {
    /// 両側にまたがるヘッダまたはメタ行。
    Full(DiffLine),
    /// 左が旧ファイル、右が新ファイル。
    Pair(Option<DiffLine>, Option<DiffLine>),
}

/// 連続する削除群と直後の追加群を先頭からペアにする。
pub fn to_split_rows(lines: &[DiffLine]) -> Vec<SplitRow> {
    use DiffLineKind::*;
    let mut rows = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            Context => {
                rows.push(SplitRow::Pair(
                    Some(lines[i].clone()),
                    Some(lines[i].clone()),
                ));
                i += 1;
            }
            Removed | Added => {
                let start = i;
                while i < lines.len() && lines[i].kind == Removed {
                    i += 1;
                }
                let middle = i;
                while i < lines.len() && lines[i].kind == Added {
                    i += 1;
                }
                let removed = &lines[start..middle];
                let added = &lines[middle..i];
                for n in 0..removed.len().max(added.len()) {
                    rows.push(SplitRow::Pair(
                        removed.get(n).cloned(),
                        added.get(n).cloned(),
                    ));
                }
            }
            _ => {
                rows.push(SplitRow::Full(lines[i].clone()));
                i += 1;
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_numbers() {
        let lines = parse_unified_diff("@@ -1,3 +1,4 @@\n a\n-b\n+B\n+C\n c\n");
        let numbers: Vec<_> = lines
            .iter()
            .map(|l| (l.kind, l.old_lineno, l.new_lineno))
            .collect();
        use DiffLineKind::*;
        assert_eq!(
            numbers,
            vec![
                (HunkHeader, None, None),
                (Context, Some(1), Some(1)),
                (Removed, Some(2), None),
                (Added, None, Some(2)),
                (Added, None, Some(3)),
                (Context, Some(3), Some(4))
            ]
        );
        assert_eq!(lines[2].text, "-b");
    }

    #[test]
    fn resets_at_second_hunk() {
        let lines = parse_unified_diff("@@ -1 +1 @@\n a\n@@ -20 +30 @@ function\n b");
        assert_eq!(
            (lines[3].old_lineno, lines[3].new_lineno),
            (Some(20), Some(30))
        );
    }

    #[test]
    fn newline_marker_is_attached_without_breaking_replacement_pair() {
        let lines = parse_unified_diff(
            "@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n\\ No newline at end of file\n",
        );
        assert_eq!(lines.len(), 3);
        assert!(lines[1].no_newline);
        assert!(lines[2].no_newline);
        assert_eq!(lines[1].old_lineno, Some(1));
        assert_eq!(lines[2].new_lineno, Some(1));
        assert_eq!(
            to_split_rows(&lines)[1],
            SplitRow::Pair(Some(lines[1].clone()), Some(lines[2].clone()))
        );
        assert!(
            matches!(&to_split_rows(&parse_unified_diff("\\ No newline at end of file"))[0], SplitRow::Full(line) if line.kind == DiffLineKind::Meta)
        );
    }

    #[test]
    fn real_git_newline_markers_keep_the_correct_side() {
        use std::process::Command;
        let directory =
            std::env::temp_dir().join(format!("eren-newline-diff-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        for (old, new, old_missing, new_missing) in [
            ("old", "new", true, true),
            ("old", "new\n", true, false),
            ("old\n", "new", false, true),
            ("old\n", "new\n", false, false),
        ] {
            std::fs::write(directory.join("old"), old).unwrap();
            std::fs::write(directory.join("new"), new).unwrap();
            let output = Command::new("git")
                .args(["diff", "--no-index", "--", "old", "new"])
                .current_dir(&directory)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1));
            let lines = parse_unified_diff(&String::from_utf8(output.stdout).unwrap());
            let rows = to_split_rows(&lines);
            let pairs: Vec<_> = rows
                .iter()
                .filter_map(|row| match row {
                    SplitRow::Pair(Some(old), Some(new)) => Some((old, new)),
                    _ => None,
                })
                .collect();
            assert_eq!(pairs.len(), 1, "{rows:?}");
            assert_eq!(pairs[0].0.text, "-old");
            assert_eq!(pairs[0].1.text, "+new");
            assert_eq!(pairs[0].0.no_newline, old_missing);
            assert_eq!(pairs[0].1.no_newline, new_missing);
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn markers_do_not_pair_across_hunks_or_file_headers() {
        let lines = parse_unified_diff("@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n@@ -4 +4 @@\n-c\n+d\n\\ No newline at end of file\ndiff --git a/next b/next\n@@ -1 +1 @@\n-e\n+f");
        let rows = to_split_rows(&lines);
        let pairs: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                SplitRow::Pair(Some(old), Some(new)) => {
                    Some((old.text.as_str(), new.text.as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(pairs, [("-a", "+b"), ("-c", "+d"), ("-e", "+f")]);
    }

    #[test]
    fn split_unequal_changes() {
        let lines = parse_unified_diff("@@ -1,2 +1 @@\n-a\n-b\n+c");
        assert_eq!(
            to_split_rows(&lines)[1..],
            [
                SplitRow::Pair(Some(lines[1].clone()), Some(lines[3].clone())),
                SplitRow::Pair(Some(lines[2].clone()), None)
            ]
        );
    }

    #[test]
    fn split_context_and_headers() {
        let lines = parse_unified_diff("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n a");
        let rows = to_split_rows(&lines);
        for i in 0..4 {
            assert_eq!(rows[i], SplitRow::Full(lines[i].clone()));
        }
        assert_eq!(
            rows[4],
            SplitRow::Pair(Some(lines[4].clone()), Some(lines[4].clone()))
        );
    }

    #[test]
    fn empty_input() {
        assert!(parse_unified_diff("").is_empty());
        assert!(to_split_rows(&[]).is_empty());
    }

    #[test]
    fn header_like_content_and_new_file() {
        let lines = parse_unified_diff(
            "@@ -1 +1 @@\n--- old\n+++ new\n--- a/next\n+++ b/next\n@@ -0,0 +1 @@\n+new",
        );
        assert_eq!(lines[1].kind, DiffLineKind::Removed);
        assert_eq!(lines[2].kind, DiffLineKind::Added);
        assert_eq!(lines[3].kind, DiffLineKind::FileHeader);
        assert_eq!(lines[6].new_lineno, Some(1));
    }

    #[test]
    fn split_additions_and_boundaries() {
        let lines = parse_unified_diff("+a\n+b\n meta\n-c\n@@ -4 +5 @@\n+d");
        let rows = to_split_rows(&lines);
        assert_eq!(rows[0], SplitRow::Pair(None, Some(lines[0].clone())));
        assert_eq!(rows[1], SplitRow::Pair(None, Some(lines[1].clone())));
        assert_eq!(rows[3], SplitRow::Pair(Some(lines[3].clone()), None));
        assert_eq!(rows[4], SplitRow::Full(lines[4].clone()));
        assert_eq!(rows[5], SplitRow::Pair(None, Some(lines[5].clone())));
    }

    #[test]
    fn crlf_and_invalid_hunk() {
        let lines = parse_unified_diff("@@ -2 +3 @@\r\n a\r\n@@ invalid\n b");
        assert_eq!(lines[1].text, " a");
        assert_eq!(
            (lines[1].old_lineno, lines[1].new_lineno),
            (Some(2), Some(3))
        );
        assert_eq!((lines[3].old_lineno, lines[3].new_lineno), (None, None));
    }
}
