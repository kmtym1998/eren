//! Link detection for a single line of terminal output.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    Url(String),
    FilePath {
        path: String,
        line: Option<usize>,
        column: Option<usize>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    pub start: usize,
    pub end: usize,
    pub target: LinkTarget,
}

/// Finds links using UTF-8 byte offsets. URL spans take precedence over paths.
pub fn find_links(line: &str) -> Vec<LinkSpan> {
    let mut links = Vec::new();
    let mut cursor = 0;
    while cursor < line.len() {
        let rest = &line[cursor..];
        let Some(relative) = rest
            .find("http://")
            .into_iter()
            .chain(rest.find("https://"))
            .min()
        else {
            break;
        };
        let start = cursor + relative;
        let end = line[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '<' | '>'))
            .map_or(line.len(), |n| start + n);
        let mut trimmed = end;
        while trimmed > start {
            let last = line.as_bytes()[trimmed - 1];
            let opening = match last {
                b')' => Some(b'('),
                b']' => Some(b'['),
                b'}' => Some(b'{'),
                _ => None,
            };
            let unmatched = opening.is_some_and(|open| {
                let mut balance = 0usize;
                for &b in &line.as_bytes()[start..trimmed - 1] {
                    if b == open {
                        balance += 1;
                    }
                    if b == last {
                        balance = balance.saturating_sub(1);
                    }
                }
                balance == 0
            });
            if matches!(last, b'.' | b',' | b';' | b':' | b'!' | b'?') || unmatched {
                trimmed -= 1;
            } else {
                break;
            }
        }
        links.push(LinkSpan {
            start,
            end: trimmed,
            target: LinkTarget::Url(line[start..trimmed].to_owned()),
        });
        cursor = end;
    }

    let mut offset = 0;
    for token in line.split_whitespace() {
        let start = offset + line[offset..].find(token).unwrap();
        offset = start + token.len();
        if token.contains("://") || token.contains('@') {
            continue;
        }
        let wrapper = |c: char| {
            matches!(
                c,
                '\'' | '"' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
            )
        };
        let left = token.trim_start_matches(wrapper);
        let candidate = left.trim_end_matches(|c: char| {
            wrapper(c) || matches!(c, '.' | ',' | ';' | ':' | '!' | '?')
        });
        let start = start + token.len() - left.len();
        let end = start + candidate.len();
        if candidate.is_empty()
            || links
                .iter()
                .any(|link| start < link.end && link.start < end)
        {
            continue;
        }
        if let Some(target) = file_target(candidate) {
            links.push(LinkSpan { start, end, target });
        }
    }
    links.sort_by_key(|link| link.start);
    links
}

fn file_target(candidate: &str) -> Option<LinkTarget> {
    fn suffix(value: &str) -> Option<(&str, &str)> {
        let (head, tail) = value.rsplit_once(':')?;
        (!tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit())).then_some((head, tail))
    }
    let mut path = candidate;
    let mut line = None;
    let mut column = None;
    if let Some((head, number)) = suffix(path) {
        let number = number.parse::<usize>().ok()?;
        path = head;
        if let Some((head, first)) = suffix(path) {
            column = Some(number);
            line = Some(first.parse::<usize>().ok()?);
            path = head;
        } else {
            line = Some(number);
        }
    }
    const EXTENSIONS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "toml", "md", "txt", "css", "scss",
        "html", "htm", "xml", "yml", "yaml", "py", "go", "c", "h", "cpp", "hpp", "cc", "java",
        "rb", "php", "sh", "bash", "zsh", "lock", "gradle", "svelte", "vue",
    ];
    if !path.chars().any(char::is_alphanumeric) {
        return None;
    }
    if !path.contains('/')
        && !path
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !stem.is_empty() && EXTENSIONS.contains(&ext))
    {
        return None;
    }
    Some(LinkTarget::FilePath {
        path: path.to_owned(),
        line,
        column,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str, expected: &str) {
        let start = text.find(expected).unwrap();
        assert_eq!(
            find_links(text),
            vec![LinkSpan {
                start,
                end: start + expected.len(),
                target: LinkTarget::Url(expected.into())
            }]
        );
    }
    fn file(text: &str, visible: &str, path: &str, line: Option<usize>, column: Option<usize>) {
        let start = text.find(visible).unwrap();
        assert_eq!(
            find_links(text),
            vec![LinkSpan {
                start,
                end: start + visible.len(),
                target: LinkTarget::FilePath {
                    path: path.into(),
                    line,
                    column
                }
            }]
        );
    }
    #[test]
    fn plain_url() {
        url("visit https://example.com", "https://example.com");
    }
    #[test]
    fn trailing_period() {
        url("http://example.com.", "http://example.com");
    }
    #[test]
    fn balanced_parentheses() {
        url("https://example.com/a(b)", "https://example.com/a(b)");
    }
    #[test]
    fn unmatched_parenthesis() {
        url("(https://example.com/a(b)).", "https://example.com/a(b)");
    }
    #[test]
    fn other_brackets() {
        url(
            "https://example.com/[x]/{y}]}!?",
            "https://example.com/[x]/{y}",
        );
    }
    #[test]
    fn line_suffix() {
        file(
            "src/main.rs:12",
            "src/main.rs:12",
            "src/main.rs",
            Some(12),
            None,
        );
    }
    #[test]
    fn column_suffix() {
        file(
            "src/main.rs:12:5",
            "src/main.rs:12:5",
            "src/main.rs",
            Some(12),
            Some(5),
        );
    }
    #[test]
    fn bare_filename() {
        file("main.rs", "main.rs", "main.rs", None, None);
    }
    #[test]
    fn bare_word() {
        assert!(find_links("hello world").is_empty());
    }
    #[test]
    fn at_sign() {
        assert!(find_links("user@host/a.rs name@host.rs").is_empty());
    }
    #[test]
    fn multiple_sorted() {
        let text = "src/a.rs:1 https://example.com/b.rs other.ts:2:3";
        let links = find_links(text);
        assert_eq!(links.len(), 3);
        assert!(links.windows(2).all(|pair| pair[0].end <= pair[1].start));
        assert!(matches!(links[1].target, LinkTarget::Url(_)));
        assert_eq!(&text[links[2].start..links[2].end], "other.ts:2:3");
    }
    #[test]
    fn utf8_offsets() {
        file(
            "你好 🦀 src/main.rs:7",
            "src/main.rs:7",
            "src/main.rs",
            Some(7),
            None,
        );
        url("é 🦀 https://example.com/你好.", "https://example.com/你好");
    }
    #[test]
    fn quoted_tokens() {
        for text in [
            "'src/a.rs:2'",
            "\"src/a.rs:2\"",
            "`src/a.rs:2`",
            "[src/a.rs:2]",
        ] {
            file(text, "src/a.rs:2", "src/a.rs", Some(2), None);
        }
    }
    #[test]
    fn empty_and_punctuation() {
        assert!(find_links("").is_empty());
        assert!(find_links(":12:5 ... / ./ []").is_empty());
    }
    #[test]
    fn trailing_colon() {
        file("src/a.rs:12:", "src/a.rs:12", "src/a.rs", Some(12), None);
    }
    #[test]
    fn slash_paths() {
        for path in ["a/b/c", "./run.sh", "/tmp/x.txt"] {
            file(path, path, path, None, None);
        }
    }
    #[test]
    fn url_delimiters() {
        url("\"https://example.com\"", "https://example.com");
        url("<https://example.com>", "https://example.com");
    }
    #[test]
    fn zero_and_leading_zero() {
        file("a.rs:000:0", "a.rs:000:0", "a.rs", Some(0), Some(0));
    }
    #[test]
    fn overflowing_suffix() {
        assert!(find_links("a.rs:999999999999999999999999999999999999999999").is_empty());
    }
}
