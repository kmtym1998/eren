use crate::file_io;
use std::{
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::Arc,
};

#[derive(Clone, Copy)]
pub enum ReadKind {
    Check,
    Reload,
}
pub struct DiskContents {
    pub text: String,
    pub hash: u64,
    pub changed: bool,
}
pub struct ExternalRead {
    pub path: PathBuf,
    pub document: (u64, u64),
    pub kind: ReadKind,
    pub result: Result<DiskContents, String>,
}
impl ExternalRead {
    pub fn read(path: PathBuf, document: (u64, u64), kind: ReadKind, baseline: Arc<[u8]>) -> Self {
        let result = file_io::read_document(&path)
            .map(|text| {
                let changed = text.as_bytes() != baseline.as_ref();
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                text.hash(&mut hasher);
                DiskContents {
                    text,
                    hash: hasher.finish(),
                    changed,
                }
            })
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => "開いているファイルが外部で削除された".to_string(),
                _ => format!("再読み込みできなかった: {error}"),
            });
        Self {
            path,
            document,
            kind,
            result,
        }
    }
}
