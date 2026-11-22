use crate::{background_task::Cancellation, file_io};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct SearchMatch {
    pub path: PathBuf,
    pub rel: String,
    pub line: u32,
    pub col: u32,
    pub text: String,
}
#[derive(Default)]
pub struct SearchResults {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
    pub skipped: usize,
}

pub fn search_column(line: &str, lowercase_needle: &str) -> Option<u32> {
    let byte_index = line.to_lowercase().find(lowercase_needle)?;
    let mut lowercase_start = 0;
    for (column, ch) in line.chars().enumerate() {
        let lowercase_len: usize = ch.to_lowercase().map(char::len_utf8).sum();
        if byte_index < lowercase_start + lowercase_len {
            return Some(column as u32);
        }
        lowercase_start += lowercase_len;
    }
    Some(line.chars().count() as u32)
}

pub fn search(root: &Path, query: &str, cancel: Cancellation) -> SearchResults {
    let mut results = SearchResults::default();
    let needle = query.to_lowercase();
    let walker = ignore::WalkBuilder::new(root)
        .sort_by_file_path(|a, b| a.cmp(b))
        .build();
    for entry in walker {
        if cancel.is_cancelled() {
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                results.skipped += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        // metadata 後に拡大しても読み込みは上限で止まる。UTF-8 と NUL も共通で検証する。
        let text = match file_io::read_document(path) {
            Ok(text) => text,
            Err(_) => {
                results.skipped += 1;
                continue;
            }
        };
        for (index, line) in text.lines().enumerate() {
            if cancel.is_cancelled() {
                return results;
            }
            if let Some(col) = search_column(line, &needle) {
                results.matches.push(SearchMatch {
                    rel: path
                        .strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .into_owned(),
                    path: path.to_path_buf(),
                    line: index as u32,
                    col,
                    text: line.trim().chars().take(140).collect(),
                });
                if results.matches.len() == 300 {
                    results.truncated = true;
                    return results;
                }
            }
        }
    }
    results
}
