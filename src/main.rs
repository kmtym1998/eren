use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::button::*;
use gpui_kit::component::highlighter::Language;
use gpui_kit::component::input::{
    Editor, EditorState, HoverProvider, Input, InputEvent, InputState, Position, Rope, TabSize,
};
use gpui_kit::component::{Disableable, Icon, Sizable, StyledExt, Theme, ThemeMode, TitleBar};
use gpui_kit::*;

mod lsp;
mod terminal;

use terminal::TerminalSession;

actions!(
    eren,
    [
        SaveFile,
        ToggleEditor,
        GotoDefinition,
        GotoImplementation,
        FindReferences,
        NavigateBack,
        NavigateForward
    ]
);

/// ターミナル表示に使う等幅フォント。
#[cfg(target_os = "macos")]
const MONO_FONT: &str = "Menlo";
#[cfg(not(target_os = "macos"))]
const MONO_FONT: &str = "DejaVu Sans Mono";

// ペインの階層を背景色で分け、選択と操作にだけ青を使う。
const CANVAS: u32 = 0x1b1e24;
const SIDEBAR: u32 = 0x22262e;
const SURFACE: u32 = 0x282e38;
const LINE: u32 = 0x343b47;
const TEXT: u32 = 0xe2e7ef;
const MUTED: u32 = 0x98a3b4;
const ACCENT: u32 = 0xb4c9ee;

/// ファイルツリーの1エントリ。フォルダの children は開いたときに遅延で読む。
struct TreeEntry {
    path: PathBuf,
    name: String,
    is_dir: bool,
    children: Option<Vec<TreeEntry>>,
}

/// ツリーで見えている1行（描画用にフラット化したもの）。
struct TreeRow {
    path: PathBuf,
    name: String,
    depth: usize,
    is_dir: bool,
    expanded: bool,
}

/// 未対応の文字コードは変換せず拒否し、保存による元バイト列の破損を防ぐ。
fn decode_editor_text(bytes: Vec<u8>) -> Result<String, &'static str> {
    if bytes.contains(&0) {
        return Err("バイナリファイルは開けない。");
    }
    String::from_utf8(bytes)
        .map_err(|_| "UTF-8 以外の文字コードは編集できない。元のファイルは変更していない。")
}

const MAX_FILE_BYTES: u64 = 2_000_000;
/// ツリーに出さないフォルダ。
const SKIPPED_DIRS: [&str; 3] = [".git", "target", "node_modules"];

fn read_dir_entries(dir: &Path) -> Vec<TreeEntry> {
    let mut entries: Vec<TreeEntry> = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let is_dir = file_type.is_dir();
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_dir && SKIPPED_DIRS.contains(&name.as_str()) {
                continue;
            }
            entries.push(TreeEntry {
                path,
                name,
                is_dir,
                children: None,
            });
        }
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

fn flatten_tree(
    entries: &[TreeEntry],
    depth: usize,
    expanded: &HashSet<PathBuf>,
    rows: &mut Vec<TreeRow>,
) {
    for entry in entries {
        let is_expanded = entry.is_dir && expanded.contains(&entry.path);
        rows.push(TreeRow {
            path: entry.path.clone(),
            name: entry.name.clone(),
            depth,
            is_dir: entry.is_dir,
            expanded: is_expanded,
        });
        if is_expanded {
            if let Some(children) = &entry.children {
                flatten_tree(children, depth + 1, expanded, rows);
            }
        }
    }
}

/// そのフォルダの children を未読込なら読み込む。
fn ensure_children(entries: &mut [TreeEntry], dir: &Path) -> bool {
    for entry in entries.iter_mut() {
        if entry.path == dir {
            if entry.is_dir && entry.children.is_none() {
                entry.children = Some(read_dir_entries(&entry.path));
            }
            return true;
        }
        if let Some(children) = &mut entry.children {
            if ensure_children(children, dir) {
                return true;
            }
        }
    }
    false
}

/// `git status --porcelain=v1 -z` の出力をパースする。
/// リネーム／コピーは次の NUL 区切りが変更前パスになるため読み飛ばす。
fn parse_porcelain(stdout: &[u8]) -> Vec<ChangedFile> {
    let text = String::from_utf8_lossy(stdout);
    let mut files = Vec::new();
    let mut entries = text.split('\0').peekable();
    while let Some(entry) = entries.next() {
        let mut chars = entry.chars();
        let (Some(index_status), Some(worktree_status), Some(' ')) =
            (chars.next(), chars.next(), chars.next())
        else {
            continue;
        };
        let path: String = chars.collect();
        if path.is_empty() {
            continue;
        }
        if matches!(index_status, 'R' | 'C') || matches!(worktree_status, 'R' | 'C') {
            entries.next();
        }
        files.push(ChangedFile {
            path,
            index_status,
            worktree_status,
        });
    }
    files
}

/// .git/HEAD からブランチ名を読む（worktree 等で読めなければ None）。
fn git_branch(root: &Path) -> Option<String> {
    let head = std::fs::read_to_string(root.join(".git").join("HEAD")).ok()?;
    let head = head.trim();
    head.strip_prefix("ref: refs/heads/")
        .map(|name| name.to_string())
}

pub struct Workbench {
    project: Option<PathBuf>,
    picker_error: Option<String>,
    tree: Vec<TreeEntry>,
    expanded: HashSet<PathBuf>,
    branch: Option<String>,
    editor: Entity<EditorState>,
    editing_path: Option<PathBuf>,
    dirty: bool,
    /// 未保存のまま切り替えようとした先のファイル。確認バーで決着するまで開かない。
    pending_path: Option<PathBuf>,
    file_error: Option<String>,
    status_message: Option<String>,
    _editor_subscription: Subscription,
    /// 中央ペインの主役であるターミナル（プロジェクトを開くと起動する）。
    terminal: Option<TerminalSession>,
    terminal_error: Option<String>,
    terminal_focus: FocusHandle,
    /// エディタ（ツリー＋編集領域）を表示しているか。既定は非表示で、ターミナルがメイン。
    editor_open: bool,
    /// git status の変更ファイル一覧（相対パスと XY ステータス）。
    changes: Vec<ChangedFile>,
    changes_error: Option<String>,
    /// diff 表示中のファイル（相対パス）とその unified diff。
    diff_target: Option<String>,
    diff_text: Option<String>,
    diff_scroll: ScrollHandle,
    /// プロジェクト横断検索の入力欄と結果。
    search_input: Entity<InputState>,
    search_query: String,
    search_results: Vec<SearchMatch>,
    search_truncated: bool,
    _search_subscription: Subscription,
    /// 検索結果から開くときのジャンプ先（未保存確認を挟む場合に保持する）。
    pending_position: Option<Position>,
    /// 言語サーバー（rust-analyzer）のクライアント。起動はバックグラウンドで行い、
    /// 準備ができたらここに入る。
    lsp: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<lsp::LspClient>>>>,
    /// 言語サーバーの起動に失敗した場合の理由。
    lsp_error: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// LSP に didOpen 済みのファイル。
    lsp_open_docs: HashSet<PathBuf>,
    /// ジャンプ候補（複数ヒット時）とジャンプ元の位置。
    goto_candidates: Option<(Vec<GotoCandidate>, (PathBuf, Position))>,
    /// コードジャンプの戻る/進む履歴。
    nav_back: Vec<(PathBuf, Position)>,
    nav_forward: Vec<(PathBuf, Position)>,
    /// hover プロバイダへ渡す、現在エディタで開いている Rust ファイル。
    hover_doc: std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>,
    /// 現在のファイルの診断（言語サーバーから届いたもの）。
    diag_entries: Vec<DiagEntry>,
    /// 最後に取り込んだ診断カウンタの値。
    last_diag_version: u64,
}

/// 定義・実装ジャンプの候補1件。
pub struct GotoCandidate {
    path: PathBuf,
    position: Position,
    label: String,
}

/// エディタの hover 表示を言語サーバーへ橋渡しするプロバイダ。
struct LspHoverProvider {
    lsp: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<lsp::LspClient>>>>,
    doc: std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>,
}

impl HoverProvider for LspHoverProvider {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Option<lsp_types::Hover>>> {
        let Some(client) = self.lsp.lock().unwrap().clone() else {
            return Task::ready(Ok(None));
        };
        let Some(path) = self.doc.lock().unwrap().clone() else {
            return Task::ready(Ok(None));
        };
        if !client.supports("hoverProvider") {
            return Task::ready(Ok(None));
        }
        let text = text.to_string();
        cx.background_spawn(async move {
            match client.hover(&path, &text, offset) {
                Ok(Some(value)) => Ok(Some(lsp_types::Hover {
                    contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                        kind: lsp_types::MarkupKind::Markdown,
                        value,
                    }),
                    range: None,
                })),
                Ok(None) => Ok(None),
                Err(error) => Err(anyhow::anyhow!(error)),
            }
        })
    }
}

/// 診断（エラー・警告）1件。表示とクリックジャンプに使う。
pub struct DiagEntry {
    position: Position,
    severity: u64,
    message: String,
}

/// （行, 列）位置 → バイトオフセット。列は文字数（GPUI の Position の流儀）。
fn char_position_to_offset(text: &str, line: u32, character: u32) -> usize {
    let mut current_line = 0u32;
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
    let mut offset = line_start;
    for (count, (index, ch)) in text[line_start..].char_indices().enumerate() {
        if ch == '\n' || count as u32 >= character {
            break;
        }
        offset = line_start + index + ch.len_utf8();
    }
    offset
}

/// バイトオフセット → （行, 列）位置。列は文字数。
fn offset_to_char_position(text: &str, byte_offset: usize) -> Position {
    let mut line = 0u32;
    let mut character = 0u32;
    for (index, ch) in text.char_indices() {
        if index >= byte_offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += 1;
        }
    }
    Position::new(line, character)
}

/// rust-analyzer の実行パス。GUI 起動時は PATH に無いことがあるので実体を探す。
/// rustup の shim は環境変数に依存して失敗することがあるため、toolchain の実体を優先する。
fn rust_analyzer_program() -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if let Ok(entries) = std::fs::read_dir(home.join(".rustup/toolchains")) {
            let mut toolchains: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            toolchains.sort();
            for toolchain in toolchains {
                let candidate = toolchain.join("bin/rust-analyzer");
                if candidate.exists() {
                    return candidate.to_string_lossy().into_owned();
                }
            }
        }
        let shim = home.join(".cargo/bin/rust-analyzer");
        if shim.exists() {
            return shim.to_string_lossy().into_owned();
        }
    }
    "rust-analyzer".to_string()
}

/// 横断検索のヒット1件。
pub struct SearchMatch {
    path: PathBuf,
    rel: String,
    line: u32,
    col: u32,
    text: String,
}

/// git status --porcelain の1エントリ。
pub struct ChangedFile {
    path: String,
    index_status: char,
    worktree_status: char,
}

impl ChangedFile {
    /// 表示用の代表ステータス文字と色。
    fn badge(&self) -> (String, u32) {
        let code = format!("{}{}", self.index_status, self.worktree_status);
        let color = if code.contains('D') {
            0xe06c5f // 削除: 赤
        } else if code.contains('?') || code.contains('A') {
            0x7fb069 // 追加・未追跡: 緑
        } else if code.contains('R') {
            0x6aa6e8 // リネーム: 青
        } else {
            0xe0a458 // 変更: 琥珀
        };
        (code, color)
    }
}

fn icon(name: Glyph) -> Icon {
    Icon::new(name).size(px(16.))
}

fn section_heading(title: &'static str) -> Div {
    div()
        .px_4()
        .pt_6()
        .pb_3()
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(MUTED))
        .child(title)
}

fn panel_header(name: impl Into<SharedString>, glyph: Glyph) -> Div {
    div()
        .h_flex()
        .h(px(46.))
        .flex_shrink_0()
        .items_center()
        .gap_2()
        .px_4()
        .border_b_1()
        .border_color(rgb(LINE))
        .text_color(rgb(MUTED))
        .child(icon(glyph))
        .child(div().text_color(rgb(TEXT)).child(name.into()))
}

impl Workbench {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let lsp_slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let lsp_error_slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let hover_doc_slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .line_number(true)
                .indent_guides(true)
                .tab_size(TabSize {
                    tab_size: 4,
                    hard_tabs: false,
                })
                .soft_wrap(false)
                .placeholder("ファイルを選ぶと、ここで編集できる")
        });
        {
            let provider = std::rc::Rc::new(LspHoverProvider {
                lsp: std::sync::Arc::clone(&lsp_slot),
                doc: std::sync::Arc::clone(&hover_doc_slot),
            });
            editor.update(cx, |state, _cx| {
                state.lsp_mut().hover_provider = Some(provider);
            });
        }
        let subscription = cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                if !this.dirty {
                    this.dirty = true;
                    cx.notify();
                }
                this.sync_lsp_change(cx);
            }
        });
        let search_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("検索して Enter（プロジェクト内）"));
        let search_subscription =
            cx.subscribe(&search_input, |this, state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let query = state.read(cx).value().to_string();
                    this.run_search(&query, cx);
                }
            });
        // ターミナルの出力更新（Wakeup フラグ）を定期的に拾って再描画する。
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(40))
                .await;
            let keep = this
                .update(cx, |this, cx| {
                    let mut changed = this.refresh_diagnostics(cx);
                    if this.terminal.as_ref().is_some_and(|session| {
                        session.process_events();
                        session
                            .wake
                            .swap(false, std::sync::atomic::Ordering::SeqCst)
                    }) {
                        changed = true;
                    }
                    if changed {
                        cx.notify();
                    }
                })
                .is_ok();
            if !keep {
                break;
            }
        })
        .detach();
        Self {
            project: None,
            picker_error: None,
            tree: Vec::new(),
            expanded: HashSet::new(),
            branch: None,
            editor,
            editing_path: None,
            dirty: false,
            pending_path: None,
            file_error: None,
            status_message: None,
            _editor_subscription: subscription,
            terminal: None,
            terminal_error: None,
            terminal_focus: cx.focus_handle(),
            editor_open: false,
            changes: Vec::new(),
            changes_error: None,
            diff_target: None,
            diff_text: None,
            diff_scroll: ScrollHandle::new(),
            search_input,
            search_query: String::new(),
            search_results: Vec::new(),
            search_truncated: false,
            _search_subscription: search_subscription,
            pending_position: None,
            lsp: lsp_slot,
            lsp_error: lsp_error_slot,
            lsp_open_docs: HashSet::new(),
            goto_candidates: None,
            nav_back: Vec::new(),
            nav_forward: Vec::new(),
            hover_doc: hover_doc_slot,
            diag_entries: Vec::new(),
            last_diag_version: 0,
        }
    }

    fn load_project(&mut self, root: PathBuf) {
        self.tree = read_dir_entries(&root);
        self.branch = git_branch(&root);
        self.expanded.clear();
        self.editing_path = None;
        self.dirty = false;
        self.pending_path = None;
        self.file_error = None;
        self.status_message = None;
        self.editor_open = false;
        self.diff_target = None;
        self.diff_text = None;
        self.search_query = String::new();
        self.search_results = Vec::new();
        self.search_truncated = false;
        self.pending_position = None;
        self.lsp_open_docs.clear();
        *self.hover_doc.lock().unwrap() = None;
        self.diag_entries = Vec::new();
        self.last_diag_version = 0;
        self.goto_candidates = None;
        self.nav_back.clear();
        self.nav_forward.clear();
        *self.lsp.lock().unwrap() = None;
        *self.lsp_error.lock().unwrap() = None;
        // Rust プロジェクトなら言語サーバーをバックグラウンドで起動する。
        if root.join("Cargo.toml").exists() {
            let slot = std::sync::Arc::clone(&self.lsp);
            let error_slot = std::sync::Arc::clone(&self.lsp_error);
            let root_for_thread = root.clone();
            std::thread::spawn(move || {
                match lsp::LspClient::spawn(&rust_analyzer_program(), &[], &root_for_thread) {
                    Ok(client) => {
                        *slot.lock().unwrap() = Some(std::sync::Arc::new(client));
                    }
                    Err(error) => {
                        *error_slot.lock().unwrap() = Some(error);
                    }
                }
            });
        }
        // ターミナルはプロジェクトのフォルダを起点に起動し直す。
        if let Some(old) = &self.terminal {
            old.shutdown();
        }
        match TerminalSession::new(&root, 100, 30) {
            Ok(session) => {
                self.terminal = Some(session);
                self.terminal_error = None;
            }
            Err(error) => {
                self.terminal = None;
                self.terminal_error = Some(format!("ターミナルを起動できなかった: {error}"));
            }
        }
        self.project = Some(root);
        self.refresh_changes();
    }

    /// git status を読み直して変更一覧を更新する。
    fn refresh_changes(&mut self) {
        let Some(root) = self.project.clone() else {
            return;
        };
        let output = std::process::Command::new("git")
            .args(["status", "--porcelain=v1", "-z", "--untracked-files=normal"])
            .current_dir(&root)
            .output();
        match output {
            Ok(out) if out.status.success() => {
                self.changes_error = None;
                self.changes = parse_porcelain(&out.stdout);
            }
            Ok(out) => {
                self.changes = Vec::new();
                let stderr = String::from_utf8_lossy(&out.stderr);
                self.changes_error = Some(if stderr.contains("not a git repository") {
                    "Git リポジトリではないため、変更は表示できない".to_string()
                } else {
                    "git status を実行できなかった".to_string()
                });
            }
            Err(_) => {
                self.changes = Vec::new();
                self.changes_error = Some("git コマンドが見つからない".to_string());
            }
        }
    }

    /// 変更ファイルの diff を開く（エディタ領域に表示する）。
    fn open_diff(&mut self, file: &ChangedFile, cx: &mut Context<Self>) {
        let Some(root) = self.project.clone() else {
            return;
        };
        let untracked = file.index_status == '?' && file.worktree_status == '?';
        let output = if untracked {
            // 未追跡ファイルは /dev/null との diff で全行追加として見せる。
            std::process::Command::new("git")
                .args(["diff", "--no-index", "--", "/dev/null", &file.path])
                .current_dir(&root)
                .output()
        } else {
            std::process::Command::new("git")
                .args(["diff", "HEAD", "--", &file.path])
                .current_dir(&root)
                .output()
        };
        match output {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout).into_owned();
                self.diff_text = Some(if text.trim().is_empty() {
                    "差分はない（内容が HEAD と同じか、バイナリファイル）。".to_string()
                } else {
                    text
                });
                self.diff_target = Some(file.path.clone());
                self.editor_open = true;
            }
            Err(_) => {
                self.diff_text = Some("git diff を実行できなかった。".to_string());
                self.diff_target = Some(file.path.clone());
                self.editor_open = true;
            }
        }
        cx.notify();
    }

    fn on_tree_click(
        &mut self,
        path: PathBuf,
        is_dir: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if is_dir {
            if self.expanded.contains(&path) {
                self.expanded.remove(&path);
            } else {
                ensure_children(&mut self.tree, &path);
                self.expanded.insert(path);
            }
        } else {
            // 開いているファイル自身のクリックは再読込しない（未保存の編集を守る）。
            if self.editing_path.as_ref() == Some(&path) {
                cx.notify();
                return;
            }
            // 未保存のまま別ファイルへ行くときは、すぐ開かず確認バーを出す。
            if self.dirty {
                self.pending_path = Some(path);
                self.pending_position = None;
                cx.notify();
                return;
            }
            self.open_path(path, window, cx);
        }
        cx.notify();
    }

    fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_path_at(path, None, window, cx);
    }

    /// ファイルを開く。`jump` があればその位置（0 始まりの行・列）へカーソルを移動する。
    fn open_path_at(
        &mut self,
        path: PathBuf,
        jump: Option<Position>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.file_error = None;
        self.status_message = None;
        self.pending_path = None;
        self.pending_position = None;
        self.diff_target = None;
        self.diff_text = None;
        // ファイルを開く操作ではエディタを表示する。
        self.editor_open = true;
        match std::fs::metadata(&path) {
            Ok(meta) if meta.len() > MAX_FILE_BYTES => {
                self.file_error = Some("ファイルが大きいため開けない（上限 2 MB）。".into());
                self.editing_path = None;
                return;
            }
            Ok(_) => {}
            Err(_) => {
                self.file_error = Some("ファイルを開けなかった。".into());
                self.editing_path = None;
                return;
            }
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => {
                self.file_error = Some("ファイルを読み込めなかった。".into());
                self.editing_path = None;
                return;
            }
        };
        let text = match decode_editor_text(bytes) {
            Ok(text) => text,
            Err(error) => {
                self.file_error = Some(error.into());
                self.editing_path = None;
                return;
            }
        };
        let language = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| Language::from_str(ext).name().to_string())
            .unwrap_or_else(|| "plain".to_string());
        self.editor.update(cx, |state, cx| {
            state.set_highlighter(language, cx);
            state.set_value(text.clone(), window, cx);
        });
        // 言語サーバーが起動していれば、このファイルを開いたことを伝える。
        if path.extension().is_some_and(|ext| ext == "rs") {
            let client = self.lsp.lock().unwrap().clone();
            if let Some(client) = client {
                client.did_open(&path, "rust", &text);
                self.lsp_open_docs.insert(path.clone());
            }
            *self.hover_doc.lock().unwrap() = Some(path.clone());
        } else {
            *self.hover_doc.lock().unwrap() = None;
        }
        // 診断表示は新しいファイル用に取り直す。
        self.last_diag_version = u64::MAX;
        self.editing_path = Some(path);
        self.dirty = false;
        if let Some(position) = jump {
            self.editor.update(cx, |state, cx| {
                state.set_cursor_position(position, window, cx);
            });
        } else {
            let focus_handle = self.editor.focus_handle(cx);
            focus_handle.focus(window, cx);
        }
    }

    /// 編集中のファイルを保存する。保存不要・保存成功なら true、失敗なら false。
    fn save_current(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(path) = self.editing_path.clone() else {
            return false;
        };
        if !self.dirty {
            return true;
        }
        let text = self.editor.read(cx).text().to_string();
        match std::fs::write(&path, text) {
            Ok(()) => {
                self.dirty = false;
                self.status_message = Some("保存した".into());
                self.refresh_changes();
                cx.notify();
                true
            }
            Err(error) => {
                self.status_message = Some(format!("保存できなかった: {error}"));
                cx.notify();
                false
            }
        }
    }

    fn save_file(&mut self, cx: &mut Context<Self>) {
        self.save_current(cx);
    }

    /// 確認バーで「保存して開く」が選ばれたとき。保存に成功したら待機中のファイルを開く。
    fn save_and_open_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_path.take() else {
            return;
        };
        let position = self.pending_position.take();
        if self.save_current(cx) {
            self.open_path_at(pending, position, window, cx);
        } else {
            self.pending_path = Some(pending);
            self.pending_position = position;
        }
        cx.notify();
    }

    /// 確認バーで「破棄して開く」が選ばれたとき。未保存の編集は捨てて開く。
    fn discard_and_open_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pending) = self.pending_path.take() {
            let position = self.pending_position.take();
            self.open_path_at(pending, position, window, cx);
        }
        cx.notify();
    }

    /// プロジェクト内を横断検索する（部分一致、大文字小文字を区別しない）。
    fn run_search(&mut self, query: &str, cx: &mut Context<Self>) {
        let query = query.trim().to_string();
        self.search_query = query.clone();
        self.search_results = Vec::new();
        self.search_truncated = false;
        if query.is_empty() {
            cx.notify();
            return;
        }
        let Some(root) = self.project.clone() else {
            cx.notify();
            return;
        };
        let needle = query.to_lowercase();
        let mut results: Vec<SearchMatch> = Vec::new();
        let walker = ignore::WalkBuilder::new(&root)
            .sort_by_file_path(|a, b| a.cmp(b))
            .build();
        'outer: for entry in walker.flatten() {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = entry.path().to_path_buf();
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if bytes.iter().take(8192).any(|b| *b == 0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (index, line) in text.lines().enumerate() {
                if let Some(byte_idx) = line.to_lowercase().find(&needle) {
                    let col = line[..byte_idx].chars().count() as u32;
                    results.push(SearchMatch {
                        rel: path
                            .strip_prefix(&root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .into_owned(),
                        path: path.clone(),
                        line: index as u32,
                        col,
                        text: line.trim().chars().take(140).collect(),
                    });
                    if results.len() >= 300 {
                        self.search_truncated = true;
                        break 'outer;
                    }
                }
            }
        }
        self.search_results = results;
        cx.notify();
    }

    /// 編集内容を言語サーバーへ全文同期する（didOpen 済みのファイルだけ）。
    fn sync_lsp_change(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.editing_path.clone() else {
            return;
        };
        if !self.lsp_open_docs.contains(&path) {
            return;
        }
        let client = self.lsp.lock().unwrap().clone();
        if let Some(client) = client {
            let text = self.editor.read(cx).text().to_string();
            client.did_change(&path, &text);
        }
    }

    /// 言語サーバーから届いた診断を取り込む。変化があれば true。
    fn refresh_diagnostics(&mut self, cx: &App) -> bool {
        let client = self.lsp.lock().unwrap().clone();
        let Some(client) = client else {
            return false;
        };
        let version = client.diagnostics_version();
        if version == self.last_diag_version {
            return false;
        }
        self.last_diag_version = version;
        let Some(path) = self.editing_path.clone() else {
            self.diag_entries = Vec::new();
            return true;
        };
        let text = self.editor.read(cx).text().to_string();
        self.diag_entries = client
            .diagnostics_for(&path)
            .iter()
            .filter_map(|diag| {
                let start = diag.pointer("/range/start")?;
                let line = start.get("line")?.as_u64()? as u32;
                let character = start.get("character")?.as_u64()? as u32;
                let offset = lsp::position_to_offset(&text, line, character);
                Some(DiagEntry {
                    position: offset_to_char_position(&text, offset),
                    severity: diag.get("severity").and_then(|s| s.as_u64()).unwrap_or(1),
                    message: diag.get("message")?.as_str()?.replace('\n', " "),
                })
            })
            .collect();
        true
    }

    fn goto_definition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.goto("textDocument/definition", "定義", window, cx);
    }

    fn find_references(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.goto("textDocument/references", "参照", window, cx);
    }

    fn goto_implementation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.goto("textDocument/implementation", "実装", window, cx);
    }

    /// 定義・実装ジャンプ共通処理。サーバーへの問い合わせはバックグラウンドで行う。
    fn goto(
        &mut self,
        method: &str,
        kind_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.editing_path.clone() else {
            return;
        };
        if self.diff_target.is_some() {
            return;
        }
        let client = self.lsp.lock().unwrap().clone();
        let Some(client) = client else {
            if let Some(error) = self.lsp_error.lock().unwrap().clone() {
                self.status_message = Some(format!("言語サーバーを起動できなかった: {error}"));
            } else {
                self.status_message =
                    Some("言語サーバーは起動中。少し待ってからもう一度試して".into());
            }
            cx.notify();
            return;
        };
        let capability = if method.ends_with("definition") {
            "definitionProvider"
        } else if method.ends_with("implementation") {
            "implementationProvider"
        } else {
            "referencesProvider"
        };
        if !client.supports(capability) {
            self.status_message = Some(format!(
                "この言語サーバーは{kind_label}ジャンプに対応していない"
            ));
            cx.notify();
            return;
        }
        let text = self.editor.read(cx).text().to_string();
        let cursor = self.editor.read(cx).cursor_position();
        let offset = char_position_to_offset(&text, cursor.line, cursor.character);
        let origin = (path.clone(), cursor);
        let method_owned = method.to_string();
        let label = kind_label.to_string();
        let task = cx.background_spawn(async move {
            if method_owned == "textDocument/references" {
                client.references(&path, &text, offset)
            } else {
                client.goto(&method_owned, &path, &text, offset)
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.apply_goto(result, &label, origin, window, cx);
            });
        })
        .detach();
    }

    /// ジャンプ結果の反映：0件はメッセージ、1件は即ジャンプ、複数は候補リストを出す。
    fn apply_goto(
        &mut self,
        result: Result<Vec<lsp::LspLocation>, String>,
        kind_label: &str,
        origin: (PathBuf, Position),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Err(error) => {
                self.status_message = Some(format!("ジャンプできなかった: {error}"));
            }
            Ok(locations) if locations.is_empty() => {
                self.status_message = Some(format!("{kind_label}が見つからなかった"));
            }
            Ok(mut locations) if locations.len() == 1 => {
                let location = locations.remove(0);
                self.jump_to_location(&location, origin, window, cx);
            }
            Ok(locations) => {
                let mut candidates = Vec::new();
                for location in &locations {
                    if let Some(position) = self.location_position(location, cx) {
                        let label = format!(
                            "{}:{}",
                            location
                                .path
                                .strip_prefix(self.project.as_deref().unwrap_or(&location.path))
                                .unwrap_or(&location.path)
                                .to_string_lossy(),
                            location.line + 1
                        );
                        candidates.push(GotoCandidate {
                            path: location.path.clone(),
                            position,
                            label,
                        });
                    }
                }
                if candidates.is_empty() {
                    self.status_message = Some(format!("{kind_label}が見つからなかった"));
                } else {
                    self.goto_candidates = Some((candidates, origin));
                }
            }
        }
        cx.notify();
    }

    /// LSP の位置（列は UTF-16）をエディタの位置（列は文字数）へ変換する。
    fn location_position(&self, location: &lsp::LspLocation, cx: &App) -> Option<Position> {
        let text = if self.editing_path.as_deref() == Some(location.path.as_path()) {
            self.editor.read(cx).text().to_string()
        } else {
            std::fs::read_to_string(&location.path).ok()?
        };
        let offset = lsp::position_to_offset(&text, location.line, location.character);
        Some(offset_to_char_position(&text, offset))
    }

    /// ジャンプ実行。履歴にジャンプ元を積み、対象を開く（未保存時は確認バーを経由）。
    fn jump_to_location(
        &mut self,
        location: &lsp::LspLocation,
        origin: (PathBuf, Position),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(position) = self.location_position(location, cx) else {
            return;
        };
        if origin.0 != location.path
            || origin.1.line != position.line
            || origin.1.character != position.character
        {
            self.nav_back.push(origin);
            self.nav_forward.clear();
        }
        self.goto_candidates = None;
        self.jump_to(location.path.clone(), position, window, cx);
    }

    /// 履歴を積まずに (path, position) へ移動する。
    fn jump_to(
        &mut self,
        path: PathBuf,
        position: Position,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing_path.as_deref() == Some(path.as_path()) {
            self.diff_target = None;
            self.diff_text = None;
            self.editor_open = true;
            self.editor.update(cx, |state, cx| {
                state.set_cursor_position(position, window, cx);
            });
        } else if self.dirty {
            self.pending_path = Some(path);
            self.pending_position = Some(position);
        } else {
            self.open_path_at(path, Some(position), window, cx);
        }
        cx.notify();
    }

    fn navigate_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, position)) = self.nav_back.last().cloned() else {
            return;
        };
        if self.dirty && self.editing_path.as_deref() != Some(path.as_path()) {
            self.pending_path = Some(path);
            self.pending_position = Some(position);
            cx.notify();
            return;
        }
        self.nav_back.pop();
        if let Some(current) = self.editing_path.clone() {
            let cursor = self.editor.read(cx).cursor_position();
            self.nav_forward.push((current, cursor));
        }
        self.goto_candidates = None;
        self.jump_to(path, position, window, cx);
    }

    fn navigate_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, position)) = self.nav_forward.last().cloned() else {
            return;
        };
        if self.dirty && self.editing_path.as_deref() != Some(path.as_path()) {
            self.pending_path = Some(path);
            self.pending_position = Some(position);
            cx.notify();
            return;
        }
        self.nav_forward.pop();
        if let Some(current) = self.editing_path.clone() {
            let cursor = self.editor.read(cx).cursor_position();
            self.nav_back.push((current, cursor));
        }
        self.goto_candidates = None;
        self.jump_to(path, position, window, cx);
    }

    /// 検索結果をクリックしたとき：該当ファイルを開いて行へジャンプする。
    fn open_search_match(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.search_results.get(index) else {
            return;
        };
        let (path, position) = (m.path.clone(), Position::new(m.line, m.col));
        // 開いているファイル自身なら、再読込せずカーソルだけ動かす（未保存の編集を守る）。
        if self.editing_path.as_deref() == Some(path.as_path()) {
            self.diff_target = None;
            self.diff_text = None;
            self.editor_open = true;
            self.editor.update(cx, |state, cx| {
                state.set_cursor_position(position, window, cx);
            });
            cx.notify();
            return;
        }
        if self.dirty {
            self.pending_path = Some(path);
            self.pending_position = Some(position);
            cx.notify();
            return;
        }
        self.open_path_at(path, Some(position), window, cx);
    }

    /// フォルダ選択後も、未保存の編集を解決するまで現在のプロジェクトを保持する。
    fn request_project(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dirty {
            self.load_project(root);
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            "未保存の変更がある",
            Some("プロジェクトを切り替える前に、編集中のファイルを保存する？"),
            &[
                PromptButton::Ok("保存して開く".into()),
                PromptButton::Other("破棄して開く".into()),
                PromptButton::Cancel("キャンセル".into()),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let answer = answer.await;
            let _ = this.update_in(cx, |this, _, cx| {
                match answer {
                    Ok(0) if this.save_current(cx) => this.load_project(root),
                    Ok(1) => this.load_project(root),
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("プロジェクトを開く".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = paths.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(root) = paths.into_iter().next() {
                            this.request_project(root, window, cx);
                        }
                        this.picker_error = None;
                    }
                    Ok(Ok(None)) => {}
                    _ => {
                        this.picker_error =
                            Some("フォルダを開けなかった。もう一度選択してね。".into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn sidebar(&self, cx: &Context<Self>) -> Div {
        let name = self
            .project
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "プロジェクト未選択".into());

        div()
            .v_flex()
            .w(px(232.))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(panel_header("ワークスペース", Glyph::PanelsTopLeft))
            .child(section_heading("プロジェクト"))
            .child(
                div()
                    .mx_2()
                    .px_3()
                    .py_3()
                    .rounded_md()
                    .bg(rgb(SURFACE))
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .child(icon(Glyph::Folder).text_color(rgb(ACCENT)))
                    .child(div().flex_1().min_w_0().overflow_hidden().child(name)),
            )
            .child(
                div()
                    .px_4()
                    .pt_3()
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child(if self.project.is_some() {
                        "このフォルダを作業の起点にする"
                    } else {
                        "フォルダを開いて作業を始める"
                    }),
            )
            .child(section_heading("エージェント"))
            .child(
                div()
                    .px_4()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .text_color(rgb(MUTED))
                    .child(div().size(px(6.)).rounded_full().bg(rgb(0x697586)))
                    .child("接続されたエージェントなし"),
            )
            .child(div().flex_1())
            .child(
                div()
                    .px_4()
                    .py_4()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .child(
                        Button::new("sidebar-open")
                            .ghost()
                            .small()
                            .icon(Glyph::FolderOpen)
                            .label("フォルダを開く")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_project(window, cx)),
                            ),
                    ),
            )
    }

    fn tree_panel(&self, cx: &Context<Self>) -> Div {
        let mut rows: Vec<TreeRow> = Vec::new();
        flatten_tree(&self.tree, 0, &self.expanded, &mut rows);

        let mut list = div()
            .id("tree-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .py_2();
        if rows.is_empty() {
            list = list.child(
                div()
                    .px_4()
                    .py_3()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child("表示するファイルがない"),
            );
        }
        for (index, row) in rows.iter().enumerate() {
            let selected = self
                .editing_path
                .as_ref()
                .is_some_and(|path| path == &row.path);
            let glyph = if row.is_dir {
                if row.expanded {
                    Glyph::FolderOpen
                } else {
                    Glyph::Folder
                }
            } else {
                Glyph::File
            };
            let mut item = div()
                .id(("tree-row", index as u64))
                .h_flex()
                .items_center()
                .gap_1()
                .h(px(26.))
                .pl(px(8. + row.depth as f32 * 14.))
                .pr_2()
                .cursor_pointer()
                .hover(|style| style.bg(rgb(SURFACE)))
                .child(if row.is_dir {
                    icon(if row.expanded {
                        Glyph::ChevronDown
                    } else {
                        Glyph::ChevronRight
                    })
                    .size(px(12.))
                    .text_color(rgb(MUTED))
                } else {
                    Icon::new(Glyph::ChevronRight)
                        .size(px(12.))
                        .text_color(rgb(CANVAS))
                })
                .child(icon(glyph).size(px(14.)).text_color(if row.is_dir {
                    rgb(ACCENT)
                } else {
                    rgb(MUTED)
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(row.name.clone()),
                );
            if selected {
                item = item.bg(rgb(SURFACE)).text_color(rgb(ACCENT));
            }
            let path = row.path.clone();
            let is_dir = row.is_dir;
            list = list.child(item.on_click(cx.listener(move |this, _, window, cx| {
                this.on_tree_click(path.clone(), is_dir, window, cx);
            })));
        }

        let mut header = panel_header("ファイル", Glyph::Folder);
        if let Some(branch) = &self.branch {
            header = header.child(div().flex_1()).child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(11.))
                    .child(icon(Glyph::GitBranch).size(px(12.)))
                    .child(branch.clone()),
            );
        }

        let mut panel = div()
            .v_flex()
            .w(px(264.))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(header)
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(Input::new(&self.search_input).small()),
            );
        if self.search_query.is_empty() {
            panel = panel.child(list);
        } else {
            panel = panel.child(self.search_results_view(cx));
        }
        panel
    }

    /// 横断検索の結果リスト（検索中はツリーの代わりに出す）。
    fn search_results_view(&self, cx: &Context<Self>) -> Div {
        let mut summary = div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .text_size(px(11.))
            .text_color(rgb(MUTED))
            .child(format!(
                "{} 件{}",
                self.search_results.len(),
                if self.search_truncated {
                    "（上限で打ち切り）"
                } else {
                    ""
                }
            ));
        summary = summary.child(div().flex_1()).child(
            Button::new("clear-search")
                .ghost()
                .small()
                .label("クリア")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.search_query = String::new();
                    this.search_results = Vec::new();
                    this.search_truncated = false;
                    this.search_input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                    });
                    cx.notify();
                })),
        );
        let mut view = div().v_flex().flex_1().min_h_0().child(summary);
        if self.search_results.is_empty() {
            view = view.child(
                div()
                    .px_4()
                    .py_3()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child("見つからなかった"),
            );
        } else {
            let mut list = div()
                .id("search-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .py_1();
            for (index, m) in self.search_results.iter().enumerate() {
                let item = div()
                    .id(("search-row", index as u64))
                    .v_flex()
                    .gap(px(1.))
                    .px_3()
                    .py(px(5.))
                    .mx_2()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(SURFACE)))
                    .child(
                        div()
                            .h_flex()
                            .items_baseline()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(12.))
                                    .child(m.rel.clone()),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .font_family(MONO_FONT)
                                    .text_size(px(10.))
                                    .text_color(rgb(MUTED))
                                    .child(format!(":{}", m.line + 1)),
                            ),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(11.))
                            .text_color(rgb(MUTED))
                            .child(m.text.clone()),
                    );
                list = list.child(item.on_click(cx.listener(move |this, _, window, cx| {
                    this.open_search_match(index, window, cx);
                })));
            }
            view = view.child(list);
        }
        view
    }

    fn viewer(&self, cx: &Context<Self>) -> Div {
        // diff 表示が優先（エディタの内容は保持したまま切り替える）。
        if let (Some(target), Some(text)) = (&self.diff_target, &self.diff_text) {
            return self.diff_view(target.clone(), text.clone(), cx);
        }
        let panel = div().v_flex().flex_1().min_w_0().h_full();

        if let Some(error) = &self.file_error {
            return panel.child(panel_header("エディタ", Glyph::File)).child(
                div()
                    .flex_1()
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .child(div().text_color(rgb(0xe9b4aa)).child(error.clone())),
            );
        }

        let Some(path) = &self.editing_path else {
            return panel.child(panel_header("エディタ", Glyph::File)).child(
                div()
                    .flex_1()
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_color(rgb(MUTED))
                            .child("左のツリーでファイルを選ぶと、ここで編集できる"),
                    ),
            );
        };

        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut header = panel_header(title, Glyph::File);
        if self.dirty {
            header = header.child(
                div()
                    .size(px(7.))
                    .rounded_full()
                    .bg(rgb(0xe9b4aa))
                    .flex_shrink_0(),
            );
        }
        header = header
            .child(div().flex_1())
            .child(
                Button::new("goto-definition")
                    .ghost()
                    .small()
                    .label("定義")
                    .on_click(cx.listener(|this, _, window, cx| this.goto_definition(window, cx))),
            )
            .child(
                Button::new("goto-implementation")
                    .ghost()
                    .small()
                    .label("実装")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.goto_implementation(window, cx)),
                    ),
            )
            .child(
                Button::new("find-references")
                    .ghost()
                    .small()
                    .label("参照")
                    .on_click(cx.listener(|this, _, window, cx| this.find_references(window, cx))),
            )
            .child(
                Button::new("save-file")
                    .ghost()
                    .small()
                    .label("保存")
                    .disabled(!self.dirty)
                    .on_click(cx.listener(|this, _, _, cx| this.save_file(cx))),
            )
            .child(
                div()
                    .id("close-editor")
                    .cursor_pointer()
                    .px_1()
                    .text_color(rgb(MUTED))
                    .child(icon(Glyph::X))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.editor_open = false;
                        this.terminal_focus.focus(window, cx);
                        cx.notify();
                    })),
            );

        let mut panel = panel.child(header);
        if let Some(_pending) = &self.pending_path {
            panel = panel.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .flex_shrink_0()
                    .bg(rgb(0x3a2e22))
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .text_size(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child("未保存の変更があります"),
                    )
                    .child(
                        Button::new("confirm-save-open")
                            .primary()
                            .small()
                            .label("保存して開く")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_and_open_pending(window, cx)
                            })),
                    )
                    .child(
                        Button::new("confirm-discard-open")
                            .ghost()
                            .small()
                            .label("破棄して開く")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.discard_and_open_pending(window, cx)
                            })),
                    )
                    .child(
                        Button::new("confirm-cancel")
                            .ghost()
                            .small()
                            .label("キャンセル")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pending_path = None;
                                cx.notify();
                            })),
                    ),
            );
        }
        if let Some((candidates, _)) = &self.goto_candidates {
            let mut list = div()
                .id("goto-candidates-scroll")
                .v_flex()
                .flex_shrink_0()
                .max_h(px(220.))
                .overflow_y_scroll()
                .py_1()
                .bg(rgb(SURFACE))
                .border_b_1()
                .border_color(rgb(LINE));
            for (index, candidate) in candidates.iter().enumerate() {
                list = list.child(
                    div()
                        .id(("goto-candidate", index as u64))
                        .px_4()
                        .py(px(5.))
                        .cursor_pointer()
                        .text_size(px(12.))
                        .hover(|style| style.bg(rgb(SIDEBAR)))
                        .child(candidate.label.clone())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some((candidates, origin)) = this.goto_candidates.take() {
                                if let Some(candidate) = candidates.into_iter().nth(index) {
                                    // 候補の位置は変換済み（文字数ベース）なので直接開く。
                                    this.nav_back.push(origin);
                                    this.nav_forward.clear();
                                    this.jump_to(candidate.path, candidate.position, window, cx);
                                }
                            }
                        })),
                );
            }
            panel = panel.child(list);
        }
        if !self.diag_entries.is_empty() {
            let mut bar = div()
                .id("diag-scroll")
                .v_flex()
                .flex_shrink_0()
                .max_h(px(132.))
                .overflow_y_scroll()
                .py_1()
                .bg(rgb(0x2a2320))
                .border_b_1()
                .border_color(rgb(LINE));
            for (index, entry) in self.diag_entries.iter().enumerate() {
                let (mark, color) = if entry.severity == 1 {
                    ("エラー", 0xe06c5a)
                } else if entry.severity == 2 {
                    ("警告", 0xe5c07b)
                } else {
                    ("情報", MUTED)
                };
                let position = entry.position;
                bar = bar.child(
                    div()
                        .id(("diag-row", index as u64))
                        .h_flex()
                        .items_baseline()
                        .gap_2()
                        .px_4()
                        .py(px(3.))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(SURFACE)))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(11.))
                                .text_color(rgb(color))
                                .child(mark),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(px(12.))
                                .child(format!("{}: {}", position.line + 1, entry.message)),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(path) = this.editing_path.clone() {
                                this.jump_to(path, position, window, cx);
                            }
                        })),
                );
            }
            panel = panel.child(bar);
        }
        panel.child(
            div().flex_1().min_h_0().child(
                Editor::new(&self.editor)
                    .bordered(false)
                    .h_full()
                    .text_size(px(12.5)),
            ),
        )
    }

    fn toggle_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor_open = !self.editor_open;
        if self.editor_open {
            if self.editing_path.is_some() {
                self.editor.update(cx, |state, cx| state.focus(window, cx));
            }
        } else {
            self.terminal_focus.focus(window, cx);
        }
        cx.notify();
    }

    /// canvas の実測サイズからターミナルの列数・行数を更新する。
    fn terminal_resize(&mut self, cols: usize, rows: usize, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.terminal {
            if session.cols != cols || session.rows != rows {
                session.resize(cols, rows);
                cx.notify();
            }
        }
    }

    fn on_terminal_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Shift+PageUp / PageDown は履歴スクロールに使う（シェルへは送らない）。
        if event.keystroke.modifiers.shift {
            let page = match event.keystroke.key.as_str() {
                "pageup" => Some(1),
                "pagedown" => Some(-1),
                _ => None,
            };
            if let Some(direction) = page {
                if let Some(session) = &self.terminal {
                    let rows = session.rows as i32;
                    session.scroll(direction * rows);
                    cx.notify();
                }
                cx.stop_propagation();
                return;
            }
        }
        if let Some(bytes) = keystroke_to_bytes(&event.keystroke) {
            if let Some(session) = &self.terminal {
                session.write(&bytes);
            }
            // Ctrl+S などアプリ側のキーバインドへは回さない（ターミナル入力を優先）。
            cx.stop_propagation();
        }
    }

    fn terminal_panel(&self, cx: &Context<Self>) -> Div {
        let mut header = panel_header("ターミナル", Glyph::Terminal);
        header = header.child(div().flex_1()).child(
            Button::new("toggle-editor")
                .ghost()
                .small()
                .label(if self.editor_open {
                    "エディタを隠す"
                } else {
                    "エディタを表示"
                })
                .on_click(cx.listener(|this, _, window, cx| this.toggle_editor(window, cx))),
        );
        let panel = div().v_flex().flex_1().min_w_0().h_full().child(header);

        if let Some(error) = &self.terminal_error {
            return panel.child(
                div()
                    .flex_1()
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .child(div().text_color(rgb(0xe9b4aa)).child(error.clone())),
            );
        }
        let Some(session) = &self.terminal else {
            return panel.child(
                div()
                    .flex_1()
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_color(rgb(MUTED))
                            .child("プロジェクトを開くとターミナルが起動する"),
                    ),
            );
        };

        let snapshot = session.snapshot();
        let default_bg = snapshot.default_bg;
        let scrolled_up = snapshot.display_offset > 0;
        let weak = cx.entity().downgrade();
        let body = div()
            .relative()
            .flex_1()
            .min_h_0()
            .bg(rgb(CANVAS))
            .overflow_hidden()
            .track_focus(&self.terminal_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_terminal_key(event, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.terminal_focus.focus(window, cx);
                }),
            )
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                let lines = match event.delta {
                    ScrollDelta::Lines(point) => point.y as i32,
                    ScrollDelta::Pixels(point) => (point.y / px(terminal::CELL_H)) as i32,
                };
                if let Some(session) = &this.terminal {
                    session.scroll(lines);
                }
                cx.notify();
            }))
            .child(
                div().absolute().inset_0().child(
                    canvas(
                        move |bounds, _, cx| {
                            // 表示側の padding（上下左右 8px ずつ）を差し引いて数える。
                            let cols = ((bounds.size.width - px(16.)) / px(terminal::CELL_W))
                                .floor() as usize;
                            let rows = ((bounds.size.height - px(16.)) / px(terminal::CELL_H))
                                .floor() as usize;
                            if cols >= 2 && rows >= 1 {
                                let _ = weak
                                    .update(cx, |this, cx| this.terminal_resize(cols, rows, cx));
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .size_full(),
                ),
            )
            .child(
                div()
                    .v_flex()
                    .p_2()
                    .font_family(MONO_FONT)
                    .text_size(px(12.5))
                    .children(snapshot.lines.iter().map(|line| {
                        div()
                            .h_flex()
                            .h(px(terminal::CELL_H))
                            .flex_shrink_0()
                            .children(line.runs.iter().map(|run| {
                                let mut run_div = div()
                                    .flex_shrink_0()
                                    .whitespace_nowrap()
                                    .w(px(run.columns as f32 * terminal::CELL_W))
                                    .text_color(rgb(run.fg))
                                    .child(run.text.clone());
                                if run.bg != default_bg {
                                    run_div = run_div.bg(rgb(run.bg));
                                }
                                if run.bold {
                                    run_div = run_div.font_weight(FontWeight::BOLD);
                                }
                                run_div
                            }))
                    })),
            );
        let mut body = body;
        if scrolled_up {
            body = body.child(
                div()
                    .absolute()
                    .bottom_2()
                    .right_3()
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .bg(rgb(SURFACE))
                    .border_1()
                    .border_color(rgb(LINE))
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child("履歴を表示中（キー入力で最新の位置へ戻る）"),
            );
        }
        panel.child(body)
    }

    fn workspace(&self, cx: &Context<Self>) -> Div {
        if self.project.is_some() {
            return div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(panel_header("作業領域", Glyph::CodeXml))
                .child({
                    let mut row = div()
                        .h_flex()
                        .flex_1()
                        .min_h_0()
                        .child(self.terminal_panel(cx));
                    if self.editor_open {
                        row = row.child(self.tree_panel(cx)).child(self.viewer(cx));
                    }
                    row
                })
                .child(
                    div()
                        .h(px(44.))
                        .h_flex()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .text_size(px(11.))
                        .text_color(rgb(MUTED))
                        .child(icon(Glyph::PencilLine))
                        .child(if let Some(message) = &self.status_message {
                            message.clone()
                        } else if self.dirty {
                            "未保存の変更がある（Cmd+S で保存）".to_string()
                        } else if self.editing_path.is_some() {
                            "保存済み".to_string()
                        } else {
                            "ファイルを選んで編集を始める".to_string()
                        }),
                );
        }
        let title = "ここから、作業を始めよう";
        let detail = "ローカルのプロジェクトを、このワークスペースに。".to_string();
        let mut empty = div()
            .v_flex()
            .items_center()
            .gap_4()
            .max_w(px(540.))
            .px_8()
            .child(
                div()
                    .size(px(68.))
                    .rounded(px(18.))
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(SIDEBAR))
                    .h_flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(Glyph::FolderOpen)
                            .size(px(30.))
                            .text_color(rgb(ACCENT)),
                    ),
            )
            .child(
                div()
                    .mt_3()
                    .text_size(px(25.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(div().text_color(rgb(MUTED)).text_center().child(detail))
            .child(
                div().mt_3().child(
                    Button::new("open-project")
                        .primary()
                        .icon(Glyph::FolderOpen)
                        .label("プロジェクトを開く")
                        .on_click(cx.listener(|this, _, window, cx| this.open_project(window, cx))),
                ),
            );
        if let Some(error) = &self.picker_error {
            empty = empty.child(div().text_color(rgb(0xe9b4aa)).child(error.clone()));
        }
        div()
            .v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(panel_header("作業領域", Glyph::CodeXml))
            .child(
                div()
                    .flex_1()
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .child(empty),
            )
            .child(
                div()
                    .h(px(44.))
                    .h_flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child(icon(Glyph::LockKeyhole))
                    .child("コードを読むための、静かな作業場所"),
            )
    }

    /// unified diff の表示パネル（エディタ領域に出す）。
    fn diff_view(&self, target: String, text: String, cx: &Context<Self>) -> Div {
        let name = target.rsplit('/').next().unwrap_or(&target).to_string();
        let mut header = panel_header(format!("差分: {name}"), Glyph::GitCompareArrows);
        header = header.child(div().flex_1()).child(
            div()
                .id("close-diff")
                .cursor_pointer()
                .px_1()
                .text_color(rgb(MUTED))
                .child(icon(Glyph::X))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.diff_target = None;
                    this.diff_text = None;
                    cx.notify();
                })),
        );
        div()
            .v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .child(
                div()
                    .id("diff-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.diff_scroll)
                    .child(
                        div()
                            .v_flex()
                            .w_full()
                            .py_2()
                            .font_family(MONO_FONT)
                            .text_size(px(12.))
                            .children(text.lines().map(|line| {
                                let (fg, bg) = diff_line_colors(line);
                                let mut row = div()
                                    .h_flex()
                                    .h(px(17.))
                                    .flex_shrink_0()
                                    .px_3()
                                    .whitespace_nowrap()
                                    .text_color(rgb(fg))
                                    .child(if line.is_empty() {
                                        " ".to_string()
                                    } else {
                                        line.to_string()
                                    });
                                if let Some(bg) = bg {
                                    row = row.bg(rgb(bg));
                                }
                                row
                            })),
                    ),
            )
    }

    fn changes(&self, cx: &Context<Self>) -> Div {
        let mut header = panel_header("変更", Glyph::GitCompareArrows);
        if !self.changes.is_empty() {
            header = header.child(
                div()
                    .px(px(6.))
                    .py(px(1.))
                    .rounded_full()
                    .bg(rgb(SURFACE))
                    .text_size(px(11.))
                    .text_color(rgb(TEXT))
                    .child(self.changes.len().to_string()),
            );
        }
        header = header.child(div().flex_1()).child(
            div()
                .id("refresh-changes")
                .cursor_pointer()
                .px_1()
                .text_color(rgb(MUTED))
                .child(icon(Glyph::RefreshCw))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.refresh_changes();
                    cx.notify();
                })),
        );

        let mut pane = div()
            .v_flex()
            .w(px(252.))
            .flex_shrink_0()
            .h_full()
            .border_l_1()
            .border_color(rgb(LINE))
            .child(header);

        if self.project.is_none() {
            pane = pane.child(
                div()
                    .px_5()
                    .pt_6()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child("プロジェクトを開くと、変更ファイルがここに出る。"),
            );
        } else if let Some(error) = &self.changes_error {
            pane = pane.child(
                div()
                    .px_5()
                    .pt_6()
                    .text_size(px(12.))
                    .text_color(rgb(0xe9b4aa))
                    .child(error.clone()),
            );
        } else if self.changes.is_empty() {
            pane = pane.child(
                div()
                    .px_5()
                    .pt_6()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child("変更はない。ワーキングツリーはクリーン。"),
            );
        } else {
            let mut list = div().v_flex().w_full().py_2();
            for (index, file) in self.changes.iter().enumerate() {
                let (badge, color) = file.badge();
                let selected = self.diff_target.as_deref() == Some(file.path.as_str());
                let path = file.path.clone();
                let mut row = div()
                    .id(("change-row", index as u64))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py(px(5.))
                    .mx_2()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .child(
                        div()
                            .flex_shrink_0()
                            .px(px(4.))
                            .rounded(px(4.))
                            .bg(rgb(SURFACE))
                            .font_family(MONO_FONT)
                            .text_size(px(10.))
                            .text_color(rgb(color))
                            .child(badge),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(12.))
                            .child(path),
                    );
                if selected {
                    row = row.bg(rgb(SURFACE));
                }
                list = list.child(row.on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(file) = this.changes.get(index) {
                        let file = ChangedFile {
                            path: file.path.clone(),
                            index_status: file.index_status,
                            worktree_status: file.worktree_status,
                        };
                        this.open_diff(&file, cx);
                    }
                })));
            }
            pane = pane.child(
                div()
                    .id("changes-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list),
            );
        }

        pane.child(div().flex_1()).child(
            div()
                .mx_4()
                .mb_5()
                .pt_4()
                .border_t_1()
                .border_color(rgb(LINE))
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .child(match (&self.branch, self.changes.len()) {
                    (Some(branch), 0) => format!("ブランチ: {branch}"),
                    (Some(branch), n) => format!("ブランチ: {branch} / 変更 {n} 件"),
                    (None, _) => "Git 連携".to_string(),
                }),
        )
    }
}

/// diff の行種別ごとの (前景色, 背景色)。
fn diff_line_colors(line: &str) -> (u32, Option<u32>) {
    if line.starts_with("diff --git")
        || line.starts_with("index ")
        || line.starts_with("---")
        || line.starts_with("+++")
    {
        (MUTED, None)
    } else if line.starts_with('+') {
        (0x9ece8a, Some(0x22331d))
    } else if line.starts_with('-') {
        (0xe08a7d, Some(0x38231f))
    } else if line.starts_with("@@") {
        (ACCENT, None)
    } else {
        (0xc6cdd8, None)
    }
}

impl Render for Workbench {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .size_full()
            .on_action(cx.listener(|this, _: &SaveFile, _, cx| this.save_file(cx)))
            .on_action(
                cx.listener(|this, _: &ToggleEditor, window, cx| this.toggle_editor(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &GotoDefinition, window, cx| {
                    this.goto_definition(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &GotoImplementation, window, cx| {
                this.goto_implementation(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &FindReferences, window, cx| {
                    this.find_references(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &NavigateBack, window, cx| this.navigate_back(window, cx)),
            )
            .on_action(cx.listener(|this, _: &NavigateForward, window, cx| {
                this.navigate_forward(window, cx)
            }))
            .bg(rgb(CANVAS))
            .text_color(rgb(TEXT))
            .font_family(".AppleSystemUIFont")
            .text_size(px(13.))
            .child(
                TitleBar::new().bg(rgb(SIDEBAR)).child(
                    div()
                        .h_flex()
                        .flex_1()
                        .items_center()
                        .justify_between()
                        .pr_4()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("eren"))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(MUTED))
                                .child("ローカルワークスペース"),
                        ),
                ),
            )
            .child(
                div()
                    .h_flex()
                    .flex_1()
                    .min_h_0()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .child(self.sidebar(cx))
                    .child(self.workspace(cx))
                    .child(self.changes(cx)),
            )
            .child(
                div()
                    .h_flex()
                    .h(px(28.))
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(SIDEBAR))
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(5.)).rounded_full().bg(rgb(ACCENT)))
                            .child(if self.project.is_some() {
                                "プロジェクト選択済み"
                            } else {
                                "プロジェクトを選択して開始"
                            }),
                    )
                    .child(format!("開発版  {}", env!("CARGO_PKG_VERSION"))),
            )
    }
}

/// キー入力をターミナルへ送るバイト列に変換する。変換できないもの（Cmd 系など）は None。
fn keystroke_to_bytes(ks: &Keystroke) -> Option<Vec<u8>> {
    let mods = &ks.modifiers;
    if mods.platform {
        return None;
    }
    if mods.control {
        let key = ks.key.as_str();
        if key.len() == 1 {
            let byte = key.as_bytes()[0];
            if byte.is_ascii_lowercase() {
                return Some(vec![byte - b'a' + 1]);
            }
            match key {
                "[" => return Some(vec![0x1b]),
                "\\" => return Some(vec![0x1c]),
                "]" => return Some(vec![0x1d]),
                " " => return Some(vec![0]),
                _ => {}
            }
        }
        if key == "enter" {
            return Some(vec![b'\r']);
        }
        return None;
    }
    let mut out = Vec::new();
    if mods.alt {
        out.push(0x1b);
    }
    if let Some(ch) = &ks.key_char {
        if !ch.is_empty() {
            out.extend_from_slice(ch.as_bytes());
            return Some(out);
        }
    }
    let seq: &[u8] = match ks.key.as_str() {
        "enter" => b"\r",
        "backspace" => &[0x7f],
        "tab" => {
            if mods.shift {
                b"\x1b[Z"
            } else {
                b"\t"
            }
        }
        "escape" => &[0x1b],
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        "pageup" => b"\x1b[5~",
        "pagedown" => b"\x1b[6~",
        "delete" => b"\x1b[3~",
        "space" => b" ",
        _ => return None,
    };
    out.extend_from_slice(seq);
    Some(out)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("eren {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "eren {}: コーディングエージェント用ネイティブワークスペース",
            env!("CARGO_PKG_VERSION")
        );
        println!("使い方: eren [--version|--help] [プロジェクトのフォルダ]");
        println!("引数なしで GUI を起動する。フォルダを渡すとそのプロジェクトを開く。");
        return;
    }
    let initial_project = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .filter(|path| path.is_dir());

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.bind_keys([
                KeyBinding::new("cmd-s", SaveFile, None),
                KeyBinding::new("ctrl-s", SaveFile, None),
                KeyBinding::new("cmd-e", ToggleEditor, None),
                KeyBinding::new("ctrl-e", ToggleEditor, None),
                KeyBinding::new("f12", GotoDefinition, None),
                KeyBinding::new("ctrl-f12", GotoImplementation, None),
                KeyBinding::new("shift-f12", FindReferences, None),
                KeyBinding::new("alt-left", NavigateBack, None),
                KeyBinding::new("alt-right", NavigateForward, None),
            ]);
            Theme::change(ThemeMode::Dark, None, cx);
            Theme::update(cx, |theme| {
                theme.primary = rgb(ACCENT).into();
                theme.primary_foreground = rgb(CANVAS).into();
                theme.background = rgb(CANVAS).into();
            });
            let bounds = Bounds::centered(None, size(px(1180.), px(760.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(940.), px(600.))),
                    ..TitleBar::window_options()
                },
                cx,
                move |window, cx| {
                    cx.new(|cx| {
                        let mut workbench = Workbench::new(window, cx);
                        if let Some(root) = initial_project {
                            workbench.load_project(root);
                        }
                        workbench
                    })
                },
            )
            .expect("ウィンドウを開けなかった");
            cx.activate(true);
        });
}

#[cfg(test)]
mod editor_safety_tests {
    use super::decode_editor_text;

    #[test]
    fn accepts_utf8_without_changing_text() {
        let text = "日本語と café\r\n";
        assert_eq!(decode_editor_text(text.as_bytes().to_vec()).unwrap(), text);
    }

    #[test]
    fn rejects_non_utf8_instead_of_replacing_bytes() {
        assert!(decode_editor_text(b"caf\xe9\n".to_vec()).is_err());
        assert!(decode_editor_text(vec![0x93, 0xfa, 0x96, 0x7b]).is_err());
    }

    #[test]
    fn rejects_nul_even_after_the_old_probe_limit() {
        let mut bytes = vec![b'a'; 9000];
        bytes.push(0);
        assert!(decode_editor_text(bytes).is_err());
    }
}
