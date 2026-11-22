//! Strict document decoding and replacement without truncating the original.
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const MAX_DOCUMENT_BYTES: u64 = 2 * 1024 * 1024;

pub fn read_document(path: &Path) -> io::Result<String> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_DOCUMENT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "通常ファイルではないか 2 MB を超えている",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_DOCUMENT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "2 MB 超またはバイナリのファイルは開けない",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "不正な UTF-8 のため開けない"))
}

pub fn conflicts(path: &Path, baseline: &[u8]) -> io::Result<bool> {
    match std::fs::File::open(path) {
        Ok(mut file) => {
            // 自分の保存済み文書が2MiBを超えていても、固定バッファで比較する。
            // 外部で巨大化した場合は長さで拒否し、読み込み中の変化も末尾まで検出する。
            if file.metadata()?.len() != baseline.len() as u64 {
                return Ok(true);
            }
            let mut buffer = [0u8; 8192];
            let mut offset = 0;
            while offset < baseline.len() {
                let length = buffer.len().min(baseline.len() - offset);
                let read = file.read(&mut buffer[..length])?;
                if read == 0 || buffer[..read] != baseline[offset..offset + read] {
                    return Ok(true);
                }
                offset += read;
            }
            Ok(file.read(&mut buffer[..1])? != 0)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn atomic_save(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_save_with(path, |file| file.write_all(bytes))
}

fn atomic_save_with(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    // Resolve symlinks so saving replaces the target, preserving the link itself.
    let target = match std::fs::canonicalize(path) {
        Ok(target) => target,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(path).is_ok() {
                return Err(io::Error::other("リンク先が見つからない"));
            }
            path.to_path_buf()
        }
        Err(error) => return Err(error),
    };
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("親ディレクトリがない"))?;
    let permissions = match std::fs::metadata(&target) {
        Ok(meta) => {
            if meta.permissions().readonly() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "読み取り専用ファイル",
                ));
            }
            Some(meta.permissions())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (temporary, mut file) = loop {
        let candidate = parent.join(format!(
            ".eren-save-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => break (Temporary(candidate), file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    write(&mut file)?;
    if let Some(permissions) = permissions {
        file.set_permissions(permissions)?;
    }
    file.sync_all()?;
    std::fs::rename(&temporary.0, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "eren-io-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        dir
    }
    #[test]
    fn strict_decoding_and_roundtrip() {
        let dir = directory();
        let path = dir.join("file");
        for bytes in [vec![255], {
            let mut b = vec![b'a'; 9000];
            b.push(0);
            b
        }] {
            std::fs::write(&path, &bytes).unwrap();
            assert!(read_document(&path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        let text = "日本語😀\r\n";
        std::fs::write(&path, text).unwrap();
        assert_eq!(read_document(&path).unwrap(), text);
        atomic_save(&path, text.as_bytes()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn partial_write_preserves_original_and_cleans_temporary() {
        let dir = directory();
        let path = dir.join("file");
        std::fs::write(&path, "original").unwrap();
        let result = atomic_save_with(&path, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected disk full"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn conflict_checks_contents_and_deletion() {
        let dir = directory();
        let path = dir.join("file");
        std::fs::write(&path, "base").unwrap();
        assert!(!conflicts(&path, b"base").unwrap());
        std::fs::write(&path, "edit").unwrap();
        assert!(conflicts(&path, b"base").unwrap());
        std::fs::remove_file(&path).unwrap();
        assert!(conflicts(&path, b"base").unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn conflict_check_accepts_large_self_saved_baseline_and_bounds_external_growth() {
        let dir = directory();
        let path = dir.join("large");
        let mut baseline = vec![b'a'; MAX_DOCUMENT_BYTES as usize + 1];
        atomic_save(&path, &baseline).unwrap();
        assert!(!conflicts(&path, &baseline).unwrap());
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"extra")
            .unwrap();
        assert!(conflicts(&path, &baseline).unwrap());
        baseline[0] = b'b';
        atomic_save(&path, &baseline).unwrap();
        baseline[0] = b'a';
        assert!(conflicts(&path, &baseline).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn preserves_symlink_and_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = directory();
        let path = dir.join("file");
        let link = dir.join("link");
        std::fs::write(&path, "base").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&path, &link).unwrap();
        atomic_save(&link, b"edit").unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(&path).unwrap(), b"edit");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
