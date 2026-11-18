//! 最小の LSP クライアント。
//!
//! 言語サーバーを子プロセスとして起動し、stdio の JSON-RPC で通信する。
//! 読み取りは専用スレッド、リクエストは呼び出し側スレッドでブロックして待つ。
//! 同期は全文同期（didOpen / didChange で全文を送る）のみ。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// ジャンプ先（LSP の Location / LocationLink を正規化したもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspLocation {
    pub path: PathBuf,
    /// 0 始まりの行。
    pub line: u32,
    /// 0 始まりの列（UTF-16 コードユニット数）。
    pub character: u32,
}

#[derive(Default)]
struct PendingState {
    requests: HashMap<i64, mpsc::Sender<Result<Value, String>>>,
    disconnected: bool,
}

fn disconnect(pending: &Mutex<PendingState>, error: &str) {
    let mut pending = pending.lock().unwrap();
    pending.disconnected = true;
    for (_, sender) in pending.requests.drain() {
        let _ = sender.send(Err(error.to_string()));
    }
}

/// 進行中の作業（workDoneProgress）1件。
#[derive(Clone, Debug)]
pub struct ProgressEntry {
    pub title: String,
    pub message: Option<String>,
    pub percentage: Option<u64>,
}

#[derive(Default)]
struct DocumentSync {
    open: Option<(String, String)>,
    versions: HashMap<String, i64>,
}

/// stdio LSP クライアント。
#[allow(dead_code)]
pub struct LspClient {
    child: Mutex<Option<Child>>,
    writer: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<PendingState>>,
    reader: Mutex<Option<std::thread::JoinHandle<()>>>,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    diagnostics_version: Arc<AtomicU64>,
    progress: Arc<Mutex<HashMap<String, ProgressEntry>>>,
    progress_version: Arc<AtomicU64>,
    documents: Mutex<DocumentSync>,
    next_id: AtomicI64,
    /// initialize の応答に含まれるサーバーの capabilities。
    pub capabilities: Value,
    pub root: PathBuf,
}

fn write_message(writer: &Arc<Mutex<ChildStdin>>, value: &Value) -> Result<(), String> {
    let body = serde_json::to_string(value).map_err(|e| e.to_string())?;
    let mut stdin = writer.lock().map_err(|_| "ロック失敗".to_string())?;
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body)
        .and_then(|()| stdin.flush())
        .map_err(|e| e.to_string())
}

/// フレーム（ヘッダ＋本文）を1つ読み取る。EOF なら None。
fn read_message(reader: &mut BufReader<impl Read>) -> Option<Value> {
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {}
            Err(_) => return None,
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    if content_length == 0 {
        return None;
    }
    let mut buf = vec![0u8; content_length];
    reader.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

#[allow(dead_code)]
impl LspClient {
    /// サーバーを起動し、initialize ハンドシェイクまで終える。
    pub fn spawn(program: &str, args: &[&str], root: &Path) -> Result<LspClient, String> {
        Self::spawn_cancellable(program, args, root, || false)
    }

    pub fn spawn_cancellable(
        program: &str,
        args: &[&str],
        root: &Path,
        cancelled: impl Fn() -> bool,
    ) -> Result<LspClient, String> {
        let mut child = Command::new(program)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("言語サーバーを起動できなかった: {e}"))?;
        let stdin = child.stdin.take().ok_or("stdin を取得できなかった")?;
        let stdout = child.stdout.take().ok_or("stdout を取得できなかった")?;
        let writer = Arc::new(Mutex::new(stdin));
        let pending = Arc::new(Mutex::new(PendingState::default()));
        let diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let diagnostics_version = Arc::new(AtomicU64::new(0));
        let progress: Arc<Mutex<HashMap<String, ProgressEntry>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let progress_version = Arc::new(AtomicU64::new(0));

        let reader = {
            let writer = Arc::clone(&writer);
            let pending = Arc::clone(&pending);
            let diagnostics = Arc::clone(&diagnostics);
            let diagnostics_version = Arc::clone(&diagnostics_version);
            let progress = Arc::clone(&progress);
            let progress_version = Arc::clone(&progress_version);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                while let Some(message) = read_message(&mut reader) {
                    let id = message.get("id").cloned();
                    let method = message
                        .get("method")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    match (id, method) {
                        (Some(id), None) => {
                            // サーバーからの応答。
                            let result = if let Some(error) = message.get("error") {
                                Err(error.to_string())
                            } else {
                                Ok(message.get("result").cloned().unwrap_or(Value::Null))
                            };
                            if let Some(key) = id.as_i64() {
                                if let Some(sender) = pending.lock().unwrap().requests.remove(&key)
                                {
                                    let _ = sender.send(result);
                                }
                            }
                        }
                        (Some(id), Some(method)) => {
                            // サーバーからのリクエストには最小限だけ応答する。
                            let result = if method == "workspace/configuration" {
                                let count = message
                                    .pointer("/params/items")
                                    .and_then(Value::as_array)
                                    .map_or(1, Vec::len);
                                Value::Array(vec![Value::Null; count])
                            } else {
                                Value::Null
                            };
                            if write_message(
                                &writer,
                                &json!({"jsonrpc": "2.0", "id": id, "result": result}),
                            )
                            .is_err()
                            {
                                break;
                            }
                        }
                        (None, Some(method)) => {
                            // 通知。診断と進捗だけ保持する。
                            if method == "textDocument/publishDiagnostics" {
                                if let Some(uri) =
                                    message.pointer("/params/uri").and_then(Value::as_str)
                                {
                                    let list = message
                                        .pointer("/params/diagnostics")
                                        .and_then(Value::as_array)
                                        .cloned()
                                        .unwrap_or_default();
                                    diagnostics.lock().unwrap().insert(uri.to_string(), list);
                                    diagnostics_version.fetch_add(1, Ordering::SeqCst);
                                }
                            } else if method == "$/progress" {
                                let token = message.pointer("/params/token").map(|token| {
                                    token
                                        .as_str()
                                        .map(str::to_string)
                                        .unwrap_or_else(|| token.to_string())
                                });
                                if let (Some(token), Some(value)) =
                                    (token, message.pointer("/params/value"))
                                {
                                    let kind =
                                        value.get("kind").and_then(Value::as_str).unwrap_or("");
                                    let mut map = progress.lock().unwrap();
                                    let changed = match kind {
                                        "begin" => {
                                            map.insert(
                                                token,
                                                ProgressEntry {
                                                    title: value
                                                        .get("title")
                                                        .and_then(Value::as_str)
                                                        .unwrap_or_default()
                                                        .to_string(),
                                                    message: value
                                                        .get("message")
                                                        .and_then(Value::as_str)
                                                        .map(str::to_string),
                                                    percentage: value
                                                        .get("percentage")
                                                        .and_then(Value::as_u64),
                                                },
                                            );
                                            true
                                        }
                                        "report" => {
                                            if let Some(entry) = map.get_mut(&token) {
                                                if let Some(message) =
                                                    value.get("message").and_then(Value::as_str)
                                                {
                                                    entry.message = Some(message.to_string());
                                                }
                                                if let Some(percentage) =
                                                    value.get("percentage").and_then(Value::as_u64)
                                                {
                                                    entry.percentage = Some(percentage);
                                                }
                                                true
                                            } else {
                                                false
                                            }
                                        }
                                        "end" => map.remove(&token).is_some(),
                                        _ => false,
                                    };
                                    if changed {
                                        progress_version.fetch_add(1, Ordering::SeqCst);
                                    }
                                }
                            }
                        }
                        (None, None) => {}
                    }
                }
                disconnect(&pending, "言語サーバーとの接続が切れた");
            })
        };

        let mut client = LspClient {
            child: Mutex::new(Some(child)),
            writer,
            pending,
            reader: Mutex::new(Some(reader)),
            diagnostics,
            diagnostics_version,
            progress,
            progress_version,
            documents: Mutex::new(DocumentSync::default()),
            next_id: AtomicI64::new(1),
            capabilities: Value::Null,
            root: root.to_path_buf(),
        };

        let root_uri = path_to_uri(root);
        let result = client.request_cancellable(
            "initialize",
            json!({
                "processId": Value::Null,
                "rootUri": root_uri,
                "workspaceFolders": [{"uri": root_uri, "name": "root"}],
                "capabilities": {
                    "textDocument": {
                        "synchronization": {"didSave": true},
                        "definition": {"linkSupport": true},
                        "implementation": {"linkSupport": true},
                        "references": {},
                        "hover": {"contentFormat": ["markdown", "plaintext"]},
                        "publishDiagnostics": {}
                    },
                    "workspace": {"configuration": true},
                    "window": {"workDoneProgress": true}
                }
            }),
            Duration::from_secs(60),
            &cancelled,
        )?;
        client.capabilities = result.get("capabilities").cloned().unwrap_or(Value::Null);
        client.notify("initialized", json!({}))?;
        Ok(client)
    }

    /// サーバーが capabilities で宣言しているかどうかをざっくり判定する。
    pub fn is_connected(&self) -> bool {
        !self.pending.lock().unwrap().disconnected
    }

    pub fn supports(&self, capability_key: &str) -> bool {
        match self.capabilities.get(capability_key) {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => true,
        }
    }

    pub fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.request_cancellable(method, params, Duration::from_secs(60), &|| false)
    }

    fn request_cancellable(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = mpsc::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.disconnected {
                return Err("言語サーバーとの接続が切れた".into());
            }
            pending.requests.insert(id, sender);
        }
        if let Err(error) = write_message(
            &self.writer,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        ) {
            disconnect(&self.pending, &error);
            return Err(error);
        }
        let deadline = Instant::now() + timeout;
        let error = loop {
            if cancelled() {
                break "言語サーバーの要求を取り消した";
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break "言語サーバーが応答しなかった";
            }
            match receiver.recv_timeout(remaining.min(Duration::from_millis(50))) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break "言語サーバーとの接続が切れた";
                }
            }
        };
        self.pending.lock().unwrap().requests.remove(&id);
        Err(error.into())
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        if self.pending.lock().unwrap().disconnected {
            return Err("言語サーバーとの接続が切れた".into());
        }
        let result = write_message(
            &self.writer,
            &json!({"jsonrpc": "2.0", "method": method, "params": params}),
        );
        if let Err(error) = &result {
            disconnect(&self.pending, error);
        }
        result
    }

    /// Arc を保持する旧要求が残っていても、プロジェクト切り替え時に終了できる。
    pub fn shutdown(&self) -> Option<std::thread::JoinHandle<()>> {
        disconnect(&self.pending, "言語サーバーを終了した");
        let child = self.child.lock().unwrap().take();
        let reader = self.reader.lock().unwrap().take();
        if child.is_none() && reader.is_none() {
            return None;
        }
        // kill は待機せず、wait と reader の join は UI スレッドの外へ渡す。
        let child = child.map(|mut child| {
            let _ = child.kill();
            child
        });
        Some(std::thread::spawn(move || {
            if let Some(mut child) = child {
                let _ = child.wait();
            }
            if let Some(reader) = reader {
                let _ = reader.join();
            }
        }))
    }

    /// 現在の一文書を同期する。再訪時は close/open、同じ内容の再送は省く。
    /// 状態更新と通知を同じロック内で実行し、通知順序を保証する。
    pub fn sync_document(&self, path: &Path, language_id: &str, text: &str) -> Result<(), String> {
        let uri = path_to_uri(path);
        let mut documents = self.documents.lock().unwrap();
        if let Some((open_uri, old_text)) = &documents.open {
            if *open_uri == uri && old_text == text {
                return Ok(());
            }
            if *open_uri != uri {
                self.notify(
                    "textDocument/didClose",
                    json!({"textDocument": {"uri": open_uri}}),
                )?;
                self.diagnostics.lock().unwrap().remove(open_uri);
                documents.open = None;
            }
        }
        let version = documents.versions.get(&uri).copied().unwrap_or(0) + 1;
        if documents.open.is_some() {
            self.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": {"uri": uri, "version": version},
                    "contentChanges": [{"text": text}]
                }),
            )?;
        } else {
            self.notify(
                "textDocument/didOpen",
                json!({"textDocument": {
                    "uri": uri, "languageId": language_id, "version": version, "text": text
                }}),
            )?;
        }
        documents.versions.insert(uri.clone(), version);
        documents.open = Some((uri, text.to_string()));
        Ok(())
    }

    pub fn close_document(&self) -> Result<(), String> {
        let mut documents = self.documents.lock().unwrap();
        if let Some((uri, _)) = &documents.open {
            self.notify(
                "textDocument/didClose",
                json!({"textDocument": {"uri": uri}}),
            )?;
            self.diagnostics.lock().unwrap().remove(uri);
            documents.open = None;
        }
        Ok(())
    }

    /// 定義・実装ジャンプ共通。method は "textDocument/definition" など。
    pub fn goto(
        &self,
        method: &str,
        path: &Path,
        text: &str,
        byte_offset: usize,
    ) -> Result<Vec<LspLocation>, String> {
        let (line, character) = offset_to_position(text, byte_offset);
        let result = self.request(
            method,
            json!({
                "textDocument": {"uri": path_to_uri(path)},
                "position": {"line": line, "character": character}
            }),
        )?;
        Ok(parse_locations(&result))
    }

    pub fn hover(
        &self,
        path: &Path,
        text: &str,
        byte_offset: usize,
    ) -> Result<Option<String>, String> {
        let (line, character) = offset_to_position(text, byte_offset);
        let result = self.request(
            "textDocument/hover",
            json!({
                "textDocument": {"uri": path_to_uri(path)},
                "position": {"line": line, "character": character}
            }),
        )?;
        if result.is_null() {
            return Ok(None);
        }
        let contents = &result["contents"];
        let text = if let Some(value) = contents.get("value").and_then(Value::as_str) {
            value.to_string()
        } else if let Some(array) = contents.as_array() {
            array
                .iter()
                .filter_map(|item| {
                    item.as_str().map(str::to_string).or_else(|| {
                        item.get("value")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            return Ok(None);
        };
        Ok(Some(text))
    }

    /// 参照検索（textDocument/references、宣言も含む）。
    pub fn references(
        &self,
        path: &Path,
        text: &str,
        byte_offset: usize,
    ) -> Result<Vec<LspLocation>, String> {
        let (line, character) = offset_to_position(text, byte_offset);
        let result = self.request(
            "textDocument/references",
            json!({
                "textDocument": {"uri": path_to_uri(path)},
                "position": {"line": line, "character": character},
                "context": {"includeDeclaration": true}
            }),
        )?;
        Ok(parse_locations(&result))
    }

    /// textDocument/documentSymbol の result（JSON）をそのまま返す。
    /// 構造の解釈は symbols モジュール側で行う。
    pub fn document_symbols(&self, path: &Path) -> Result<Value, String> {
        self.request(
            "textDocument/documentSymbol",
            json!({
                "textDocument": {"uri": path_to_uri(path)}
            }),
        )
    }

    /// 診断が更新されるたびに増えるカウンタ（UI 側のポーリング用）。
    pub fn diagnostics_version(&self) -> u64 {
        self.diagnostics_version.load(Ordering::SeqCst)
    }

    /// 進捗が更新されるたびに増えるカウンタ（UI 側のポーリング用）。
    pub fn progress_version(&self) -> u64 {
        self.progress_version.load(Ordering::SeqCst)
    }

    /// 進行中の作業一覧（タイトル順）。
    pub fn progress_snapshot(&self) -> Vec<ProgressEntry> {
        let mut entries: Vec<ProgressEntry> =
            self.progress.lock().unwrap().values().cloned().collect();
        entries.sort_by(|a, b| a.title.cmp(&b.title));
        entries
    }

    /// uri に対する最新の診断（publishDiagnostics で届いたもの）。
    pub fn diagnostics_for(&self, path: &Path) -> Vec<Value> {
        self.diagnostics
            .lock()
            .unwrap()
            .get(&path_to_uri(path))
            .cloned()
            .unwrap_or_default()
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Location / LocationLink / その配列 / null を正規化する。
fn parse_locations(result: &Value) -> Vec<LspLocation> {
    fn one(value: &Value) -> Option<LspLocation> {
        if let (Some(uri), Some(range)) =
            (value.get("uri").and_then(Value::as_str), value.get("range"))
        {
            let start = range.get("start")?;
            return Some(LspLocation {
                path: uri_to_path(uri),
                line: start.get("line")?.as_u64()? as u32,
                character: start.get("character")?.as_u64()? as u32,
            });
        }
        if let (Some(uri), Some(range)) = (
            value.get("targetUri").and_then(Value::as_str),
            value
                .get("targetSelectionRange")
                .or_else(|| value.get("targetRange")),
        ) {
            let start = range.get("start")?;
            return Some(LspLocation {
                path: uri_to_path(uri),
                line: start.get("line")?.as_u64()? as u32,
                character: start.get("character")?.as_u64()? as u32,
            });
        }
        None
    }
    if result.is_null() {
        return Vec::new();
    }
    if let Some(array) = result.as_array() {
        return array.iter().filter_map(one).collect();
    }
    one(result).into_iter().collect()
}

/// バイトオフセット → LSP 位置（行は 0 始まり、列は UTF-16 コードユニット数）。
pub fn offset_to_position(text: &str, byte_offset: usize) -> (u32, u32) {
    let mut line: u32 = 0;
    let mut character: u32 = 0;
    for (index, ch) in text.char_indices() {
        if index >= byte_offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16() as u32;
        }
    }
    (line, character)
}

/// LSP 位置 → バイトオフセット。範囲外はテキスト末尾に丸める。
pub fn position_to_offset(text: &str, line: u32, character: u32) -> usize {
    let mut current_line: u32 = 0;
    let mut line_start = 0usize;
    for (index, ch) in text.char_indices() {
        if ch == '\n' {
            if current_line == line {
                break;
            }
            current_line += 1;
            line_start = index + 1;
        }
    }
    if current_line < line {
        return text.len();
    }
    let mut utf16_count: u32 = 0;
    let mut offset = line_start;
    for (index, ch) in text[line_start..].char_indices() {
        if ch == '\n' || utf16_count >= character {
            break;
        }
        utf16_count += ch.len_utf16() as u32;
        offset = line_start + index + ch.len_utf8();
    }
    offset
}

/// ファイルパス → file:// URI（最小限のパーセントエンコード付き）。
pub fn path_to_uri(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut uri = String::from("file://");
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'~' | b'-') {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// file:// URI → ファイルパス。
pub fn uri_to_path(uri: &str) -> PathBuf {
    let stripped = uri.strip_prefix("file://").unwrap_or(uri);
    let stripped = stripped.strip_prefix("localhost").unwrap_or(stripped);
    let bytes = stripped.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                decoded.push(value);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&decoded).into_owned())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn mock_client(delay: &str) -> Arc<LspClient> {
        Arc::new(
            LspClient::spawn(
                "python3",
                &[
                    "-u",
                    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/delayed_lsp.py"),
                    delay,
                ],
                Path::new(env!("CARGO_MANIFEST_DIR")),
            )
            .expect("mock LSP"),
        )
    }

    pub(crate) fn stop_mock(client: &LspClient) {
        if let Some(reaper) = client.shutdown() {
            reaper.join().unwrap();
        }
    }

    fn wait_pending(client: &LspClient, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while client.pending.lock().unwrap().requests.len() != count {
            assert!(Instant::now() < deadline, "要求数が変わらない");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn assert_process_reaped(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let alive = Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                break;
            }
            assert!(Instant::now() < deadline, "子プロセス {pid} が残っている");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn eof_fails_all_pending_and_future_requests_immediately() {
        let client = mock_client("0");
        let waiting_client = client.clone();
        let waiting =
            std::thread::spawn(move || waiting_client.request("test/no-response", json!({})));
        wait_pending(&client, 1);
        let started = Instant::now();
        assert!(client.request("test/exit", json!({})).is_err());
        assert!(waiting.join().unwrap().is_err());
        assert!(client.request("test/after-exit", json!({})).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(client.pending.lock().unwrap().requests.is_empty());
        stop_mock(&client);
    }

    #[test]
    fn broken_pipe_drains_pending_without_waiting_for_stdout_eof() {
        let client = mock_client("0");
        client.request("test/close-input", json!({})).unwrap();
        assert!(!client.pending.lock().unwrap().disconnected);
        let started = Instant::now();
        assert!(client.request("test/write-failure", json!({})).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(client.pending.lock().unwrap().requests.is_empty());
        assert!(client.pending.lock().unwrap().disconnected);
        stop_mock(&client);
    }

    #[test]
    fn timeout_discards_late_response_and_keeps_connection_usable() {
        let client = mock_client("0");
        assert!(client
            .request_cancellable(
                "textDocument/definition",
                json!({"position": {"character": 0}}),
                Duration::from_millis(20),
                &|| false
            )
            .is_err());
        assert!(client.pending.lock().unwrap().requests.is_empty());
        std::thread::sleep(Duration::from_millis(250));
        assert!(client.request("test/notifications", json!({})).is_ok());
        assert!(client.pending.lock().unwrap().requests.is_empty());
        stop_mock(&client);
    }

    #[test]
    fn shutdown_reaps_process_and_reader_even_with_pending_arc() {
        let client = mock_client("0");
        let pid = client.child.lock().unwrap().as_ref().unwrap().id();
        let waiting_client = client.clone();
        let waiting =
            std::thread::spawn(move || waiting_client.request("test/no-response", json!({})));
        wait_pending(&client, 1);
        let started = Instant::now();
        let reaper = client.shutdown().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(waiting.join().unwrap().is_err());
        reaper.join().unwrap();
        assert!(client.reader.lock().unwrap().is_none());
        assert!(client.pending.lock().unwrap().requests.is_empty());
        assert!(client.shutdown().is_none());
        assert_process_reaped(pid);
    }

    #[test]
    fn drop_and_cancelled_initialization_release_child_processes() {
        let client = mock_client("0");
        let pid = client.child.lock().unwrap().as_ref().unwrap().id();
        drop(client);
        assert_process_reaped(pid);
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/delayed_lsp.py");
        for mode in ["cancel", "initialize-error"] {
            let pid_path =
                std::env::temp_dir().join(format!("eren-lsp-{}-{mode}.pid", std::process::id()));
            let _ = std::fs::remove_file(&pid_path);
            let started = Instant::now();
            let result = LspClient::spawn_cancellable(
                "python3",
                &[
                    "-u",
                    fixture,
                    if mode == "cancel" { "60" } else { "0" },
                    pid_path.to_str().unwrap(),
                    mode,
                ],
                Path::new(env!("CARGO_MANIFEST_DIR")),
                || {
                    mode == "cancel"
                        && (pid_path.exists() || started.elapsed() > Duration::from_secs(3))
                },
            );
            assert!(result.is_err());
            assert!(started.elapsed() < Duration::from_secs(3));
            let pid: u32 = std::fs::read_to_string(&pid_path).unwrap().parse().unwrap();
            assert_process_reaped(pid);
            std::fs::remove_file(pid_path).unwrap();
        }
    }

    #[test]
    fn document_notifications_are_balanced_and_versions_survive_reopen() {
        // 起動前に復元・編集された文書は、準備後に最新の本文を一度だけ送る。
        let client = mock_client("0.1");
        let a = Path::new("/tmp/a.rs");
        let b = Path::new("/tmp/b.rs");
        client
            .sync_document(a, "rust", "latest unsaved text")
            .unwrap();
        client
            .sync_document(a, "rust", "latest unsaved text")
            .unwrap();
        client.sync_document(a, "rust", "edited again").unwrap();
        client.sync_document(b, "rust", "file b").unwrap();
        client.sync_document(a, "rust", "edited again").unwrap();
        client.close_document().unwrap(); // プロジェクト切り替え・終了。
        client.close_document().unwrap();
        let log = client.request("test/notifications", json!({})).unwrap();
        let log: Vec<_> = log
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| {
                message["method"]
                    .as_str()
                    .unwrap()
                    .starts_with("textDocument/")
            })
            .collect();
        assert_eq!(
            log.iter()
                .map(|message| message["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "textDocument/didOpen",
                "textDocument/didChange",
                "textDocument/didClose",
                "textDocument/didOpen",
                "textDocument/didClose",
                "textDocument/didOpen",
                "textDocument/didClose"
            ]
        );
        assert_eq!(
            log[0]["params"]["textDocument"]["text"],
            "latest unsaved text"
        );
        assert_eq!(
            log[1]["params"]["contentChanges"][0]["text"],
            "edited again"
        );
        for (index, uri, version) in [(0, a, 1), (1, a, 2), (3, b, 1), (5, a, 3)] {
            assert_eq!(
                log[index]["params"]["textDocument"]["uri"],
                path_to_uri(uri)
            );
            assert_eq!(log[index]["params"]["textDocument"]["version"], version);
        }
        stop_mock(&client);
    }

    #[test]
    fn delayed_startup_cannot_replace_new_project() {
        use crate::lsp_epoch::LspEpoch;
        let epoch = Arc::new(Mutex::new(LspEpoch::default()));
        let slot = Arc::new(Mutex::new(None));
        let old_project = epoch.lock().unwrap().next_project();
        let old_epoch = epoch.clone();
        let old_slot = slot.clone();
        let old = std::thread::spawn(move || {
            let client = mock_client("0.2");
            let epoch = old_epoch.lock().unwrap();
            if epoch.is_project(old_project) {
                *old_slot.lock().unwrap() = Some(old_project);
            }
            stop_mock(&client);
        });
        let new_project = epoch.lock().unwrap().next_project();
        let client = mock_client("0");
        {
            let epoch = epoch.lock().unwrap();
            assert!(epoch.is_project(new_project));
            *slot.lock().unwrap() = Some(new_project);
        }
        old.join().unwrap();
        assert_eq!(*slot.lock().unwrap(), Some(new_project));
        stop_mock(&client);
    }

    #[test]
    fn delayed_response_is_rejected_after_new_request_and_document_switch() {
        use crate::lsp_epoch::{LspEpoch, RequestKind};
        let client = mock_client("0");
        let mut epoch = LspEpoch::default();
        let first = epoch.request(RequestKind::Goto);
        let old_client = client.clone();
        let old = std::thread::spawn(move || {
            old_client.goto("textDocument/definition", Path::new("/tmp/a.rs"), "ab", 0)
        });
        let second = epoch.request(RequestKind::Goto);
        let result = client
            .goto("textDocument/definition", Path::new("/tmp/a.rs"), "ab", 1)
            .unwrap();
        assert_eq!(result[0].line, 1);
        assert!(epoch.accepts(RequestKind::Goto, second));
        assert_eq!(old.join().unwrap().unwrap()[0].line, 0);
        assert!(!epoch.accepts(RequestKind::Goto, first));
        for kind in [RequestKind::Goto, RequestKind::Symbols, RequestKind::Hover] {
            let token = epoch.request(kind);
            let old_client = client.clone();
            let pending = std::thread::spawn(move || {
                old_client.goto("textDocument/definition", Path::new("/tmp/a.rs"), "ab", 0)
            });
            epoch.next_document(); // A → B → A must not revive A's old result.
            epoch.next_document();
            assert!(pending.join().unwrap().is_ok());
            assert!(!epoch.accepts(kind, token));
        }
        assert!(!epoch.accepts(RequestKind::Goto, second));
        stop_mock(&client);
    }

    #[test]
    fn utf16_conversion_roundtrip() {
        let text = "fn main() {\n    let s = \"あ🎉\";\n}\n";
        // 「あ」は UTF-16 で1、🎉 は2（サロゲートペア）。
        let offset = text.find('🎉').unwrap();
        let (line, character) = offset_to_position(text, offset);
        assert_eq!(line, 1);
        assert_eq!(position_to_offset(text, line, character), offset);
        let after = offset + '🎉'.len_utf8();
        let (_, character2) = offset_to_position(text, after);
        assert_eq!(character2, character + 2);
    }

    #[test]
    fn uri_roundtrip_with_japanese() {
        let path = PathBuf::from("/tmp/テスト dir/main.rs");
        let uri = path_to_uri(&path);
        assert!(uri.starts_with("file:///tmp/"));
        assert_eq!(uri_to_path(&uri), path);
    }

    /// 実サーバー（rust-analyzer）での結合テスト。通常の test では回さず、
    /// `cargo test -- --ignored` で明示実行する。
    #[test]
    #[ignore]
    fn rust_analyzer_definition() {
        let dir = std::env::temp_dir().join("eren-lsp-fixture");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let source = "fn helper() -> i32 {\n    42\n}\n\nfn main() {\n    let x = helper();\n    println!(\"{x}\");\n}\n";
        std::fs::write(dir.join("src/main.rs"), source).unwrap();

        let client = LspClient::spawn("rust-analyzer", &[], &dir).expect("spawn");
        assert!(client.supports("definitionProvider"));
        let main_rs = dir.join("src/main.rs");
        client.sync_document(&main_rs, "rust", source).unwrap();
        let call_offset = source.rfind("helper()").unwrap();
        let mut locations = Vec::new();
        for _ in 0..30 {
            locations = client
                .goto("textDocument/definition", &main_rs, source, call_offset)
                .expect("definition");
            if !locations.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        assert_eq!(locations.len(), 1, "定義が1件返るはず: {locations:?}");
        assert_eq!(locations[0].line, 0, "helper の定義は0行目");
        assert!(locations[0].path.ends_with("src/main.rs"));
    }
}
