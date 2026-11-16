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
    let mut result = Vec::new();
    let (mut old, mut new) = (None, None);
    let (mut old_left, mut new_left) = (0usize, 0usize);
    for text in text.lines() {
        let mut line = DiffLine {
            kind: Meta,
            text: text.to_owned(),
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
    fn newline_marker_is_meta() {
        let lines = parse_unified_diff("@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b");
        assert_eq!(lines[2].kind, DiffLineKind::Meta);
        assert_eq!((lines[2].old_lineno, lines[2].new_lineno), (None, None));
        assert_eq!(lines[3].new_lineno, Some(1));
        assert!(matches!(&to_split_rows(&lines)[2], SplitRow::Full(l) if l == &lines[2]));
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
