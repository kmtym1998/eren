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

mod agents;
mod diff;
mod file_io;
mod links;
mod lsp;
mod symbols;
mod terminal;
mod terminal_input;
mod worktrees;

use terminal::TerminalSession;

actions!(
    eren,
    [
        SaveFile,
        Quit,
        ToggleEditor,
        GotoDefinition,
        GotoImplementation,
        FindReferences,
        ShowSymbols,
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
/// ターミナルの選択範囲の背景色。
const SELECT_BG: u32 = 0x3d4c6e;

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

#[derive(Clone)]
enum PendingNavigation {
    Jump(PathBuf, Position),
    Back,
    Forward,
}

pub struct Workbench {
    project: Option<PathBuf>,
    picker_error: Option<String>,
    state_path: Option<PathBuf>,
    restoring_state: bool,
    tree: Vec<TreeEntry>,
    expanded: HashSet<PathBuf>,
    branch: Option<String>,
    editor: Entity<EditorState>,
    editing_path: Option<PathBuf>,
    dirty: bool,
    /// 未保存のまま切り替えようとした先のファイル。確認バーで決着するまで開かない。
    pending_path: Option<PathBuf>,
    pending_project: Option<PathBuf>,
    pending_close: bool,
    save_conflict: bool,
    baseline: Vec<u8>,
    file_error: Option<String>,
    status_message: Option<String>,
    _editor_subscription: Subscription,
    /// ターミナル／エージェントのセッション一覧とアクティブなセッション。
    sessions: Vec<SessionTab>,
    active_session: usize,
    session_error: Option<String>,
    terminal_focus: FocusHandle,
    /// ターミナルの選択範囲 (開始行, 開始列, 終了行, 終了列)。スナップショット座標。
    term_selection: Option<(usize, usize, usize, usize)>,
    /// ターミナル表示領域の原点（マウス座標→セルの変換用、canvas で実測）。
    term_origin: Option<Point<Pixels>>,
    ime: terminal_input::Composition,
    /// エディタ（ツリー＋編集領域）を表示しているか。既定は非表示で、ターミナルがメイン。
    editor_open: bool,
    /// git worktree の一覧（2件以上あるときだけサイドバーに出す）。
    worktree_list: Vec<worktrees::Worktree>,
    /// git status の変更ファイル一覧（相対パスと XY ステータス）。
    changes: Vec<ChangedFile>,
    changes_error: Option<String>,
    /// diff 表示中のファイル（相対パス）とその unified diff。
    diff_target: Option<String>,
    diff_text: Option<String>,
    /// 差分を左右分割で表示するか（false = unified）。
    diff_split: bool,
    diff_scroll: ScrollHandle,
    /// プロジェクト横断検索の入力欄と結果。
    search_input: Entity<InputState>,
    search_query: String,
    search_results: Vec<SearchMatch>,
    search_truncated: bool,
    _search_subscription: Subscription,
    /// 検索結果から開くときのジャンプ先（未保存確認を挟む場合に保持する）。
    pending_position: Option<Position>,
    pending_navigation: Option<PendingNavigation>,
    /// 言語サーバー（rust-analyzer）のクライアント。起動はバックグラウンドで行い、
    /// 準備ができたらここに入る。
    lsp: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<lsp::LspClient>>>>,
    /// 言語サーバーの起動に失敗した場合の理由。
    lsp_error: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// 起動中（または起動を試みた）言語サーバーの名前。
    lsp_server: Option<&'static str>,
    /// 開いているファイルのディスク上の状態（パス・更新時刻・内容ハッシュ）。
    /// 外部変更の検知に使う。
    disk_state: Option<(PathBuf, std::time::SystemTime, u64)>,
    /// LSP に didOpen 済みのファイル。
    lsp_open_docs: HashSet<PathBuf>,
    /// ジャンプ候補（複数ヒット時）とジャンプ元の位置。
    goto_candidates: Option<(Vec<GotoCandidate>, (PathBuf, Position))>,
    /// シンボル一覧パネル（documentSymbol の結果）。
    symbol_entries: Option<Vec<symbols::SymbolEntry>>,
    /// コードジャンプの戻る/進む履歴。
    nav_back: Vec<(PathBuf, Position)>,
    nav_forward: Vec<(PathBuf, Position)>,
    /// hover プロバイダへ渡す、現在エディタで開いている Rust ファイル。
    hover_doc: std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>,
    /// 現在のファイルの診断（言語サーバーから届いたもの）。
    /// 言語サーバーの進行中の作業（index 中など）の表示ラベル。
    lsp_progress: Vec<String>,
    last_progress_version: u64,
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

/// ターミナル／エージェントのセッション1つ。
pub struct SessionTab {
    title: String,
    session: TerminalSession,
    detector: agents::StatusDetector,
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

/// 拡張子から LSP の言語 ID を返す。対応外は None。
fn lsp_language_id(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => Some("rust"),
        Some("ts") => Some("typescript"),
        Some("tsx") => Some("typescriptreact"),
        Some("js") => Some("javascript"),
        Some("jsx") => Some("javascriptreact"),
        _ => None,
    }
}

/// 起動中の言語サーバーがその言語を扱えるか。
fn lsp_server_supports(server: Option<&str>, language: &str) -> bool {
    match server {
        Some("rust-analyzer") => language == "rust",
        Some("typescript-language-server") => matches!(
            language,
            "typescript" | "typescriptreact" | "javascript" | "javascriptreact"
        ),
        _ => false,
    }
}

/// セッション種別を保存用キーに変換する。
fn kind_key(kind: agents::AgentKind) -> &'static str {
    match kind {
        agents::AgentKind::Shell => "shell",
        agents::AgentKind::ClaudeCode => "claude",
        agents::AgentKind::Codex => "codex",
        agents::AgentKind::OpenCode => "opencode",
    }
}

/// 保存用キーからセッション種別に戻す。
fn kind_from_key(key: &str) -> Option<agents::AgentKind> {
    match key {
        "shell" => Some(agents::AgentKind::Shell),
        "claude" => Some(agents::AgentKind::ClaudeCode),
        "codex" => Some(agents::AgentKind::Codex),
        "opencode" => Some(agents::AgentKind::OpenCode),
        _ => None,
    }
}

/// 状態ファイル（プロジェクトやセッション構成の復元用）の場所。
fn state_file_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("eren")
            .join("state.json"),
    )
}

/// ファイルの更新時刻と内容ハッシュ（外部変更の検知用）。
fn read_disk_state(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hasher::write(&mut hasher, &bytes);
    Some((mtime, std::hash::Hasher::finish(&hasher)))
}

/// PATH から実行ファイルを探す。
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join(name);
        candidate.exists().then_some(candidate)
    })
}

/// 小文字化した検索位置を、エディタが使う元の行の文字単位の列へ戻す。
fn search_column(line: &str, lowercase_needle: &str) -> Option<u32> {
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
        .overflow_hidden()
        .items_center()
        .gap_2()
        .px_4()
        .border_b_1()
        .border_color(rgb(LINE))
        .text_color(rgb(MUTED))
        .child(icon(glyph))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(rgb(TEXT))
                .child(name.into()),
        )
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
        // ついでに約1秒ごとに、開いているファイルの外部変更もチェックする。
        cx.spawn_in(window, async move |this, cx| {
            let mut ticks: u32 = 0;
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(40))
                    .await;
                ticks = ticks.wrapping_add(1);
                let check_disk = ticks.is_multiple_of(25);
                let keep = this
                    .update_in(cx, |this, window, cx| {
                        let mut changed = this.refresh_diagnostics(cx);
                        if this.refresh_progress(cx) {
                            changed = true;
                        }
                        for tab in &mut this.sessions {
                            tab.session.process_events();
                            if tab
                                .session
                                .wake
                                .swap(false, std::sync::atomic::Ordering::SeqCst)
                            {
                                changed = true;
                                let text = tab.session.visible_text();
                                tab.detector.observe(&text);
                            }
                        }
                        if check_disk && this.check_external_change(window, cx) {
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
            }
        })
        .detach();
        Self {
            project: None,
            picker_error: None,
            state_path: state_file_path(),
            restoring_state: false,
            tree: Vec::new(),
            expanded: HashSet::new(),
            branch: None,
            editor,
            editing_path: None,
            dirty: false,
            pending_path: None,
            pending_project: None,
            pending_close: false,
            save_conflict: false,
            baseline: Vec::new(),
            file_error: None,
            status_message: None,
            _editor_subscription: subscription,
            sessions: Vec::new(),
            active_session: 0,
            session_error: None,
            terminal_focus: cx.focus_handle(),
            term_selection: None,
            term_origin: None,
            ime: terminal_input::Composition::default(),
            editor_open: false,
            worktree_list: Vec::new(),
            changes: Vec::new(),
            changes_error: None,
            diff_target: None,
            diff_text: None,
            diff_split: false,
            diff_scroll: ScrollHandle::new(),
            search_input,
            search_query: String::new(),
            search_results: Vec::new(),
            search_truncated: false,
            _search_subscription: search_subscription,
            pending_position: None,
            pending_navigation: None,
            lsp: lsp_slot,
            lsp_error: lsp_error_slot,
            lsp_server: None,
            disk_state: None,
            lsp_open_docs: HashSet::new(),
            goto_candidates: None,
            symbol_entries: None,
            nav_back: Vec::new(),
            nav_forward: Vec::new(),
            hover_doc: hover_doc_slot,
            lsp_progress: Vec::new(),
            last_progress_version: 0,
            diag_entries: Vec::new(),
            last_diag_version: 0,
        }
    }

    fn load_project(&mut self, root: PathBuf) {
        self.pending_project = None;
        self.pending_close = false;
        self.save_conflict = false;
        self.baseline.clear();
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
        self.diff_split = false;
        // 変換中の確定先は旧セッションのまま保持する。終了済みなら確定時に捨てる。
        self.search_query = String::new();
        self.search_results = Vec::new();
        self.search_truncated = false;
        self.pending_position = None;
        self.pending_navigation = None;
        self.disk_state = None;
        self.lsp_open_docs.clear();
        *self.hover_doc.lock().unwrap() = None;
        self.diag_entries = Vec::new();
        self.last_diag_version = 0;
        self.lsp_progress = Vec::new();
        self.last_progress_version = 0;
        self.goto_candidates = None;
        self.symbol_entries = None;
        self.nav_back.clear();
        self.nav_forward.clear();
        *self.lsp.lock().unwrap() = None;
        *self.lsp_error.lock().unwrap() = None;
        self.lsp_server = None;
        // プロジェクトの種類に応じて言語サーバーをバックグラウンドで起動する。
        let server: Option<(&'static str, String, Vec<String>)> =
            if root.join("Cargo.toml").exists() {
                Some(("rust-analyzer", rust_analyzer_program(), Vec::new()))
            } else if root.join("package.json").exists() {
                match find_on_path("typescript-language-server") {
                    Some(program) => Some((
                        "typescript-language-server",
                        program.to_string_lossy().into_owned(),
                        vec!["--stdio".to_string()],
                    )),
                    None => {
                        *self.lsp_error.lock().unwrap() =
                            Some("typescript-language-server が PATH に見つからない".to_string());
                        None
                    }
                }
            } else {
                None
            };
        if let Some((name, program, args)) = server {
            self.lsp_server = Some(name);
            let slot = std::sync::Arc::clone(&self.lsp);
            let error_slot = std::sync::Arc::clone(&self.lsp_error);
            let root_for_thread = root.clone();
            std::thread::spawn(move || {
                let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                match lsp::LspClient::spawn(&program, &arg_refs, &root_for_thread) {
                    Ok(client) => {
                        *slot.lock().unwrap() = Some(std::sync::Arc::new(client));
                    }
                    Err(error) => {
                        *error_slot.lock().unwrap() = Some(error);
                    }
                }
            });
        }
        // セッションはプロジェクトのフォルダを起点に起動し直す。
        for tab in self.sessions.drain(..) {
            tab.session.shutdown();
        }
        self.active_session = 0;
        self.term_selection = None;
        match TerminalSession::new(&root, 100, 30) {
            Ok(session) => {
                self.sessions.push(SessionTab {
                    title: "シェル".to_string(),
                    session,
                    detector: agents::StatusDetector::new(agents::AgentKind::Shell),
                });
                self.session_error = None;
            }
            Err(error) => {
                self.session_error = Some(format!("ターミナルを起動できなかった: {error}"));
            }
        }
        self.worktree_list = worktrees::list_worktrees(&root).unwrap_or_default();
        self.project = Some(root);
        self.refresh_changes();
        self.save_state();
    }

    /// 現在のレイアウト（プロジェクト、セッション構成、開いているファイル）を
    /// 状態ファイルに保存する。プロセス自体は復元せず、構成だけを次回起動時に再現する。
    fn save_state(&mut self) {
        if self.restoring_state {
            return;
        }
        let Some(project) = &self.project else {
            return;
        };
        let Some(path) = self.state_path.clone() else {
            return;
        };
        let value = serde_json::json!({
            "project": project,
            "sessions": self
                .sessions
                .iter()
                .map(|tab| kind_key(tab.detector.kind()))
                .collect::<Vec<_>>(),
            "active_session": self.active_session,
            "editing_file": self.editing_path,
            "editor_open": self.editor_open,
        });
        let result = (|| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            file_io::atomic_save(&path, value.to_string().as_bytes())
        })();
        if let Err(error) = result {
            self.status_message = Some(format!("状態を保存できなかった: {error}"));
        }
    }

    /// 前回のレイアウトを復元する（CLI 引数でプロジェクト指定がなかったとき）。
    fn restore_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.state_path.clone() else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return;
        };
        let Some(root) = value
            .get("project")
            .and_then(|item| item.as_str())
            .map(PathBuf::from)
            .filter(|dir| dir.is_dir())
        else {
            return;
        };
        self.restoring_state = true;
        self.load_project(root);
        // セッション構成を復元する（load_project が作ったシェルは作り直す）。
        let kinds: Vec<agents::AgentKind> = value
            .get("sessions")
            .and_then(|item| item.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().and_then(kind_from_key))
                    .collect()
            })
            .unwrap_or_default();
        if !kinds.is_empty() {
            for tab in self.sessions.drain(..) {
                tab.session.shutdown();
            }
            self.active_session = 0;
            for kind in &kinds {
                self.spawn_session(*kind);
            }
            let active = value
                .get("active_session")
                .and_then(|item| item.as_u64())
                .unwrap_or(0) as usize;
            self.active_session = active.min(self.sessions.len().saturating_sub(1));
        }
        if let Some(file) = value
            .get("editing_file")
            .and_then(|item| item.as_str())
            .map(PathBuf::from)
            .filter(|file| file.is_file())
        {
            self.open_path(file, window, cx);
        }
        if let Some(editor_open) = value.get("editor_open").and_then(|item| item.as_bool()) {
            self.editor_open = editor_open;
        }
        self.restoring_state = false;
        self.save_state();
        cx.notify();
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
                self.editor_open = true;
                self.diff_target = None;
                self.diff_text = None;
                self.pending_project = None;
                self.pending_close = false;
                self.pending_path = Some(path);
                self.pending_position = None;
                self.pending_navigation = None;
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
    ) -> bool {
        if self.editing_path.as_ref() == Some(&path) {
            if let Some(position) = jump {
                self.editor.update(cx, |state, cx| {
                    state.set_cursor_position(position, window, cx)
                });
            }
            self.editor_open = true;
            self.file_error = None;
            self.diff_target = None;
            self.diff_text = None;
            return true;
        }
        let text = match file_io::read_document(&path) {
            Ok(text) => text,
            Err(error) => {
                self.file_error = Some(format!("ファイルを開けなかった: {error}"));
                self.status_message = self.file_error.clone();
                cx.notify();
                return false;
            }
        };
        self.baseline = text.as_bytes().to_vec();
        self.save_conflict = false;
        self.file_error = None;
        self.status_message = None;
        self.pending_path = None;
        self.pending_position = None;
        self.pending_navigation = None;
        self.diff_target = None;
        self.diff_text = None;
        self.editor_open = true;
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
        let lsp_language = lsp_language_id(&path)
            .filter(|language| lsp_server_supports(self.lsp_server, language));
        if let Some(language) = lsp_language {
            let client = self.lsp.lock().unwrap().clone();
            if let Some(client) = client {
                client.did_open(&path, language, &text);
                self.lsp_open_docs.insert(path.clone());
            }
            *self.hover_doc.lock().unwrap() = Some(path.clone());
        } else {
            *self.hover_doc.lock().unwrap() = None;
        }
        // 診断表示は新しいファイル用に取り直す。
        self.last_diag_version = u64::MAX;
        self.disk_state = read_disk_state(&path).map(|(mtime, hash)| (path.clone(), mtime, hash));
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
        self.save_state();
        true
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
        match file_io::conflicts(&path, &self.baseline) {
            Ok(true) if !self.save_conflict => {
                self.save_conflict = true;
                self.editor_open = true;
                self.diff_target = None;
                self.status_message =
                    Some("外部変更と競合している。上書き・再読み込み・キャンセルを選ぶ".into());
                cx.notify();
                return false;
            }
            Err(error) => {
                self.status_message = Some(format!("保存前の確認に失敗: {error}"));
                cx.notify();
                return false;
            }
            _ => {}
        }
        // Only the explicit overwrite button may bypass the conflict check.
        if self.save_conflict {
            return false;
        }
        self.write_current(&path, text, cx)
    }

    fn write_current(&mut self, path: &Path, text: String, cx: &mut Context<Self>) -> bool {
        match file_io::atomic_save(path, text.as_bytes()) {
            Ok(()) => {
                self.dirty = false;
                self.baseline = text.into_bytes();
                self.save_conflict = false;
                self.disk_state =
                    read_disk_state(path).map(|(mtime, hash)| (path.to_path_buf(), mtime, hash));
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

    /// 開いているファイルが外部で変更されていたら追随する（約1秒ごとに呼ばれる）。
    /// 未保存の変更がなければ自動で読み直し、あれば上書きせず警告だけ出す。
    /// 画面に影響する変更をしたら true。
    fn check_external_change(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(path) = self.editing_path.clone() else {
            return false;
        };
        if self.diff_target.is_some() {
            return false;
        }
        let Some((stamp_path, mtime, hash)) = self.disk_state.clone() else {
            return false;
        };
        if stamp_path != path {
            return false;
        }
        match read_disk_state(&path) {
            None => {
                // 削除された。繰り返し知らせないよう記録を消して一度だけ伝える。
                self.disk_state = None;
                self.status_message = Some("開いているファイルが外部で削除された".into());
                true
            }
            Some((now_mtime, now_hash)) => {
                if now_mtime == mtime && now_hash == hash {
                    return false;
                }
                if self.dirty {
                    // 同じ変更で繰り返し知らせないよう、新しい状態を記録しておく。
                    self.disk_state = Some((path, now_mtime, now_hash));
                    self.status_message = Some(
                        "ファイルが外部で変更された（未保存の変更があるため読み直していない）"
                            .into(),
                    );
                    true
                } else {
                    self.reload_current_file(window, cx);
                    true
                }
            }
        }
    }

    /// 現在のファイルをディスクから読み直す（カーソル位置は可能な範囲で保つ）。
    fn reload_current_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.editing_path.clone() else {
            return;
        };
        let text = match file_io::read_document(&path) {
            Ok(text) => text,
            Err(error) => {
                self.status_message = Some(format!("再読み込みできなかった: {error}"));
                cx.notify();
                return;
            }
        };
        self.baseline = text.as_bytes().to_vec();
        self.save_conflict = false;
        let cursor = self.editor.read(cx).cursor_position();
        self.editor.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.set_cursor_position(cursor, window, cx);
        });
        // set_value で発火した Change により dirty が立っているので、ここで下ろす。
        self.dirty = false;
        self.disk_state = read_disk_state(&path).map(|(mtime, hash)| (path.clone(), mtime, hash));
        self.last_diag_version = u64::MAX;
        self.status_message = Some("外部で変更されたため読み直した".into());
        cx.notify();
    }

    /// 確認バーで「保存して開く」が選ばれたとき。保存に成功したら待機中のファイルを開く。
    fn save_and_open_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.save_current(cx) {
            self.continue_pending(window, cx);
        }
        cx.notify();
    }

    fn discard_and_open_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.continue_pending(window, cx);
        cx.notify();
    }

    fn cancel_pending(&mut self) {
        self.pending_path = None;
        self.pending_position = None;
        self.pending_navigation = None;
        self.pending_project = None;
        self.pending_close = false;
        self.save_conflict = false;
    }

    fn continue_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_close {
            window.remove_window();
            cx.quit();
        } else if let Some(root) = self.pending_project.take() {
            self.load_project(root);
        } else if let Some(path) = self.pending_path.take() {
            let position = self.pending_position.take().unwrap_or(Position::new(0, 0));
            let operation = self.pending_navigation.take();
            self.complete_navigation(path, position, operation, window, cx);
        }
    }

    fn request_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.dirty {
            self.pending_project = Some(root);
            self.pending_path = None;
            self.pending_position = None;
            self.pending_navigation = None;
            self.pending_close = false;
            self.editor_open = true;
            self.diff_target = None;
        } else {
            self.load_project(root);
        }
        cx.notify();
    }

    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.dirty {
            return true;
        }
        self.pending_close = true;
        self.pending_project = None;
        self.pending_path = None;
        self.pending_position = None;
        self.pending_navigation = None;
        self.editor_open = true;
        self.diff_target = None;
        self.terminal_focus.focus(window, cx);
        cx.notify();
        false
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
                if let Some(col) = search_column(line, &needle) {
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
    /// 言語サーバーの進捗（$/progress）が変わっていたら表示ラベルを取り直す。
    fn refresh_progress(&mut self, _cx: &App) -> bool {
        let client = self.lsp.lock().unwrap().clone();
        let Some(client) = client else {
            if self.lsp_progress.is_empty() {
                return false;
            }
            self.lsp_progress = Vec::new();
            return true;
        };
        let version = client.progress_version();
        if version == self.last_progress_version {
            return false;
        }
        self.last_progress_version = version;
        self.lsp_progress = client
            .progress_snapshot()
            .iter()
            .map(|entry| {
                let mut label = entry.title.clone();
                if let Some(percentage) = entry.percentage {
                    label.push_str(&format!(" {percentage}%"));
                }
                if let Some(message) = &entry.message {
                    label.push_str(&format!(": {message}"));
                }
                label
            })
            .collect();
        true
    }

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

    /// 現在のファイルのシンボル一覧を取得してパネルに出す。
    fn show_symbols(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        if !client.supports("documentSymbolProvider") {
            self.status_message = Some("この言語サーバーはシンボル表示に対応していない".into());
            cx.notify();
            return;
        }
        let task = cx.background_spawn(async move { client.document_symbols(&path) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(value) => {
                        let entries = symbols::parse_document_symbols(&value);
                        if entries.is_empty() {
                            this.status_message = Some("シンボルが見つからなかった".into());
                        } else {
                            this.symbol_entries = Some(entries);
                        }
                    }
                    Err(error) => {
                        this.status_message = Some(format!("シンボルを取得できなかった: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
        self.request_navigation(
            location.path.clone(),
            position,
            Some(PendingNavigation::Jump(origin.0, origin.1)),
            window,
            cx,
        );
    }

    /// 履歴を積まずに (path, position) へ移動する。
    fn jump_to(
        &mut self,
        path: PathBuf,
        position: Position,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_navigation(path, position, None, window, cx);
    }

    fn request_navigation(
        &mut self,
        path: PathBuf,
        position: Position,
        operation: Option<PendingNavigation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dirty && self.editing_path.as_ref() != Some(&path) {
            self.pending_project = None;
            self.pending_close = false;
            self.pending_path = Some(path);
            self.pending_position = Some(position);
            self.pending_navigation = operation;
            self.editor_open = true;
            self.diff_target = None;
        } else {
            self.complete_navigation(path, position, operation, window, cx);
        }
        cx.notify();
    }

    /// 履歴は実際の移動が成功した後に一度だけ更新する。
    fn complete_navigation(
        &mut self,
        path: PathBuf,
        position: Position,
        operation: Option<PendingNavigation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .editing_path
            .clone()
            .map(|path| (path, self.editor.read(cx).cursor_position()));
        if !self.open_path_at(path.clone(), Some(position), window, cx) {
            return;
        }
        match operation {
            Some(PendingNavigation::Back) => {
                self.nav_back.pop();
                if let Some(current) = current {
                    self.nav_forward.push(current);
                }
            }
            Some(PendingNavigation::Forward) => {
                self.nav_forward.pop();
                if let Some(current) = current {
                    self.nav_back.push(current);
                }
            }
            Some(PendingNavigation::Jump(origin, cursor))
                if origin != path || cursor != position =>
            {
                self.nav_back.push((origin, cursor));
                self.nav_forward.clear();
            }
            _ => {}
        }
        self.goto_candidates = None;
    }

    fn navigate_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, position)) = self.nav_back.last().cloned() {
            self.request_navigation(path, position, Some(PendingNavigation::Back), window, cx);
        }
    }

    fn navigate_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, position)) = self.nav_forward.last().cloned() {
            self.request_navigation(path, position, Some(PendingNavigation::Forward), window, cx);
        }
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
            self.editor_open = true;
            self.diff_target = None;
            self.diff_text = None;
            self.pending_project = None;
            self.pending_close = false;
            self.pending_path = Some(path);
            self.pending_position = Some(position);
            self.pending_navigation = None;
            cx.notify();
            return;
        }
        self.open_path_at(path, Some(position), window, cx);
    }

    fn open_project(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("プロジェクトを開く".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = paths.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(root) = paths.into_iter().next() {
                            this.request_project(root, cx);
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
            .children(self.worktrees_section(cx))
            .child(self.sessions_section(cx))
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
                            .on_click(cx.listener(|this, _, _, cx| this.open_project(cx))),
                    ),
            )
    }

    /// サイドバーの worktree 一覧（2件以上あるときだけ表示、クリックで切替）。
    fn worktrees_section(&self, cx: &Context<Self>) -> Option<Div> {
        if self.worktree_list.len() < 2 {
            return None;
        }
        let current = self.project.clone();
        let mut section = div().v_flex().child(section_heading("ワークツリー"));
        for (index, wt) in self.worktree_list.iter().enumerate() {
            let is_current = current.as_deref() == Some(wt.path.as_path());
            let folder = wt
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| wt.path.to_string_lossy().into_owned());
            let label = wt.branch.clone().unwrap_or_else(|| {
                if wt.detached {
                    "(detached)".into()
                } else {
                    folder.clone()
                }
            });
            let path = wt.path.clone();
            let mut row = div()
                .id(("worktree-row", index as u64))
                .h_flex()
                .items_center()
                .gap_2()
                .mx_2()
                .px_2()
                .py(px(5.))
                .rounded(px(6.))
                .cursor_pointer()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(12.))
                        .child(label),
                )
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(MUTED))
                        .child(folder),
                );
            if is_current {
                row = row.bg(rgb(SURFACE));
            } else {
                row = row
                    .hover(|style| style.bg(rgb(SURFACE)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.request_project(path.clone(), cx);
                        cx.notify();
                    }));
            }
            section = section.child(row);
        }
        Some(section)
    }

    /// サイドバーのセッション一覧（クリックで切替、×で終了）と追加ボタン。
    fn sessions_section(&self, cx: &Context<Self>) -> Div {
        let mut section = div().v_flex().child(section_heading("セッション"));
        if self.sessions.is_empty() {
            section = section.child(
                div()
                    .px_4()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child("プロジェクトを開くとシェルが起動する"),
            );
        }
        for (index, tab) in self.sessions.iter().enumerate() {
            let active = index == self.active_session;
            let dot_color = match tab.detector.status() {
                agents::AgentStatus::Running => 0x6cb6ff,
                agents::AgentStatus::WaitingForInput => 0xe5c07b,
                agents::AgentStatus::Starting | agents::AgentStatus::Unknown => 0x697586,
            };
            let mut row = div()
                .id(("session-row", index as u64))
                .h_flex()
                .items_center()
                .gap_2()
                .mx_2()
                .px_2()
                .py(px(5.))
                .rounded(px(6.))
                .cursor_pointer()
                .child(
                    div()
                        .size(px(7.))
                        .rounded_full()
                        .flex_shrink_0()
                        .bg(rgb(dot_color)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(12.))
                        .child(tab.title.clone()),
                );
            if active {
                row = row.bg(rgb(SURFACE));
            } else {
                row = row.hover(|style| style.bg(rgb(SURFACE)));
            }
            row = row
                .child(
                    div()
                        .id(("session-close", index as u64))
                        .cursor_pointer()
                        .px_1()
                        .text_color(rgb(MUTED))
                        .child(icon(Glyph::X))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.close_session(index, cx);
                        })),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.activate_session(index, window, cx);
                }));
            section = section.child(row);
        }
        let mut buttons = div().h_flex().flex_wrap().gap_1().px_3().pt_2();
        for kind in agents::AgentKind::all() {
            let (id, label) = match kind {
                agents::AgentKind::Shell => ("add-shell", "＋シェル".to_string()),
                agents::AgentKind::ClaudeCode => ("add-claude", kind.title().to_string()),
                agents::AgentKind::Codex => ("add-codex", kind.title().to_string()),
                agents::AgentKind::OpenCode => ("add-opencode", kind.title().to_string()),
            };
            let kind = *kind;
            buttons = buttons.child(Button::new(id).ghost().small().label(label).on_click(
                cx.listener(move |this, _, window, cx| {
                    this.add_session(kind, window, cx);
                }),
            ));
        }
        section.child(buttons)
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

        if let Some(error) = self
            .file_error
            .as_ref()
            .filter(|_| self.editing_path.is_none())
        {
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
                    .xsmall()
                    .label("定義")
                    .on_click(cx.listener(|this, _, window, cx| this.goto_definition(window, cx))),
            )
            .child(
                Button::new("goto-implementation")
                    .ghost()
                    .xsmall()
                    .label("実装")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.goto_implementation(window, cx)),
                    ),
            )
            .child(
                Button::new("find-references")
                    .ghost()
                    .xsmall()
                    .label("参照")
                    .on_click(cx.listener(|this, _, window, cx| this.find_references(window, cx))),
            )
            .child(
                Button::new("show-symbols")
                    .ghost()
                    .xsmall()
                    .label("シンボル")
                    .on_click(cx.listener(|this, _, window, cx| this.show_symbols(window, cx))),
            )
            .child(
                Button::new("save-file")
                    .ghost()
                    .xsmall()
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
        if self.pending_path.is_some() || self.pending_project.is_some() || self.pending_close {
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
                            .label("保存して続行")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_and_open_pending(window, cx)
                            })),
                    )
                    .child(
                        Button::new("confirm-discard-open")
                            .ghost()
                            .small()
                            .label("破棄して続行")
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
                                this.cancel_pending();
                                cx.notify();
                            })),
                    ),
            );
        }
        if self.save_conflict {
            panel =
                panel.child(
                    div()
                        .h_flex()
                        .gap_2()
                        .px_4()
                        .py_2()
                        .child("外部変更と競合")
                        .child(Button::new("overwrite-conflict").label("上書き").on_click(
                            cx.listener(|this, _, window, cx| {
                                let Some(path) = this.editing_path.clone() else {
                                    return;
                                };
                                let text = this.editor.read(cx).text().to_string();
                                if this.write_current(&path, text, cx) {
                                    this.continue_pending(window, cx);
                                }
                            }),
                        ))
                        .child(Button::new("reload-conflict").label("再読み込み").on_click(
                            cx.listener(|this, _, window, cx| {
                                this.reload_current_file(window, cx);
                                if !this.dirty {
                                    this.continue_pending(window, cx);
                                }
                            }),
                        ))
                        .child(Button::new("cancel-conflict").label("キャンセル").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.save_conflict = false;
                                this.cancel_pending();
                                cx.notify();
                            }),
                        )),
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
        if let Some(entries) = &self.symbol_entries {
            let mut list = div()
                .id("symbols-scroll")
                .v_flex()
                .flex_shrink_0()
                .max_h(px(220.))
                .overflow_y_scroll()
                .py_1()
                .bg(rgb(SURFACE))
                .border_b_1()
                .border_color(rgb(LINE));
            list = list.child(
                div()
                    .h_flex()
                    .items_center()
                    .px_4()
                    .py(px(3.))
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child(format!("シンボル {} 件", entries.len()))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("symbols-close")
                            .cursor_pointer()
                            .child(icon(Glyph::X))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.symbol_entries = None;
                                cx.notify();
                            })),
                    ),
            );
            for (index, entry) in entries.iter().enumerate() {
                list = list.child(
                    div()
                        .id(("symbol-row", index as u64))
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .pl(px(16. + entry.depth as f32 * 14.))
                        .pr_4()
                        .py(px(4.))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(SIDEBAR)))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(rgb(ACCENT))
                                .child(entry.kind.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(px(12.))
                                .child(entry.name.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(MUTED))
                                .child(format!(":{}", entry.line + 1)),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(entries) = this.symbol_entries.take() {
                                if let Some(entry) = entries.into_iter().nth(index) {
                                    if let Some(current) = this.editing_path.clone() {
                                        let cursor = this.editor.read(cx).cursor_position();
                                        let location = lsp::LspLocation {
                                            path: current.clone(),
                                            line: entry.line as u32,
                                            character: entry.character as u32,
                                        };
                                        this.jump_to_location(
                                            &location,
                                            (current, cursor),
                                            window,
                                            cx,
                                        );
                                    }
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
        self.save_state();
        cx.notify();
    }

    /// アクティブなセッション。
    fn terminal(&self) -> Option<&TerminalSession> {
        self.sessions
            .get(self.active_session)
            .map(|tab| &tab.session)
    }

    /// 正規化した選択範囲 (開始行, 開始列, 終了行, 終了列)。
    fn selection_range(&self) -> Option<(usize, usize, usize, usize)> {
        let (sr, sc, er, ec) = self.term_selection?;
        if (sr, sc) <= (er, ec) {
            Some((sr, sc, er, ec))
        } else {
            Some((er, ec, sr, sc))
        }
    }

    /// マウスのウィンドウ座標を表示セル (列, 行) に変換する。
    fn cell_from_position(&self, position: Point<Pixels>) -> (usize, usize) {
        let (Some(origin), Some(session)) = (self.term_origin, self.terminal()) else {
            return (0, 0);
        };
        let col = ((position.x - origin.x - px(8.)) / px(terminal::CELL_W)).floor() as i32;
        let row = ((position.y - origin.y - px(8.)) / px(terminal::CELL_H)).floor() as i32;
        (
            col.clamp(0, session.cols as i32 - 1) as usize,
            row.clamp(0, session.rows as i32 - 1) as usize,
        )
    }

    /// 選択範囲のテキスト（行末の空白は除く）。
    fn selected_text(&self) -> Option<String> {
        self.terminal()?
            .snapshot()
            .selected_text(self.selection_range()?)
    }

    /// ターミナルの (列, 行) にリンクがあれば開く。リンクを処理したら true。
    fn try_open_link(
        &mut self,
        col: usize,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (text, byte_offset) = {
            let Some(session) = self.terminal() else {
                return false;
            };
            let snapshot = session.snapshot();
            let Some(line) = snapshot.lines.get(row) else {
                return false;
            };
            let Some(byte_offset) = line.byte_offset(col) else {
                return false;
            };
            (line.text(), byte_offset)
        };
        let Some(span) = links::find_links(&text)
            .into_iter()
            .find(|span| span.start <= byte_offset && byte_offset < span.end)
        else {
            return false;
        };
        match span.target {
            links::LinkTarget::Url(url) => {
                cx.open_url(&url);
            }
            links::LinkTarget::FilePath { path, line, column } => {
                let candidate = PathBuf::from(&path);
                let resolved = if candidate.is_absolute() {
                    candidate
                } else if let Some(root) = &self.project {
                    root.join(candidate)
                } else {
                    candidate
                };
                if resolved.is_file() {
                    let jump = line.map(|l| {
                        Position::new(
                            l.saturating_sub(1) as u32,
                            column.map(|c| c.saturating_sub(1) as u32).unwrap_or(0),
                        )
                    });
                    self.jump_to(resolved, jump.unwrap_or(Position::new(0, 0)), window, cx);
                } else {
                    self.status_message = Some(format!("リンク先のファイルが見つからない: {path}"));
                    cx.notify();
                }
            }
        }
        true
    }

    /// セッションを追加する（シェルまたはエージェント）。
    /// セッションを1つ作って末尾に追加する。成功したら true。
    fn spawn_session(&mut self, kind: agents::AgentKind) -> bool {
        let Some(root) = self.project.clone() else {
            return false;
        };
        let base_title = kind.title();
        let count = self
            .sessions
            .iter()
            .filter(|tab| {
                tab.title == base_title || tab.title.starts_with(&format!("{base_title} "))
            })
            .count();
        let title = if count == 0 {
            base_title.to_string()
        } else {
            format!("{base_title} {}", count + 1)
        };
        let result = match kind.command() {
            Some(command) => TerminalSession::new_agent(&root, 100, 30, command),
            None => TerminalSession::new(&root, 100, 30),
        };
        match result {
            Ok(session) => {
                self.sessions.push(SessionTab {
                    title,
                    session,
                    detector: agents::StatusDetector::new(kind),
                });
                self.session_error = None;
                true
            }
            Err(error) => {
                self.session_error = Some(format!("セッションを起動できなかった: {error}"));
                false
            }
        }
    }

    fn add_session(
        &mut self,
        kind: agents::AgentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.spawn_session(kind) {
            self.active_session = self.sessions.len() - 1;
            self.term_selection = None;
            self.terminal_focus.focus(window, cx);
            self.save_state();
        }
        cx.notify();
    }

    fn activate_session(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index < self.sessions.len() {
            self.active_session = index;
            self.term_selection = None;
            // 変換は開始したセッションへ確定する。
            self.terminal_focus.focus(window, cx);
            self.save_state();
            cx.notify();
        }
    }

    fn close_session(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.sessions.len() {
            return;
        }
        let tab = self.sessions.remove(index);
        tab.session.shutdown();
        self.term_selection = None;
        if self.active_session >= self.sessions.len() {
            self.active_session = self.sessions.len().saturating_sub(1);
        } else if index < self.active_session {
            self.active_session -= 1;
        }
        self.save_state();
        cx.notify();
    }

    fn commit_terminal_text(&mut self, text: &str) {
        let active = self.terminal().map(TerminalSession::id);
        let target = self.ime.take_target(active);
        if let Some(session) = self
            .sessions
            .iter()
            .map(|tab| &tab.session)
            .find(|session| Some(session.id()) == target)
        {
            if !text.is_empty() {
                session.write(text.as_bytes());
            }
        }
    }

    /// canvas の実測サイズからターミナルの列数・行数を更新する。
    fn terminal_resize(
        &mut self,
        origin: Point<Pixels>,
        cols: usize,
        rows: usize,
        cx: &mut Context<Self>,
    ) {
        self.term_origin = Some(origin);
        if let Some(session) = self
            .sessions
            .get_mut(self.active_session)
            .map(|tab| &mut tab.session)
        {
            if session.cols != cols || session.rows != rows {
                session.resize(cols, rows);
                self.term_selection = None;
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
        // IME 変換中はキーを IME に任せる（確定文字列は入力ハンドラ経由で届く）。
        // stop_propagation しないことでプラットフォーム側の変換処理が進む。
        if !self.ime.text.is_empty() {
            return;
        }
        // コピー（Cmd+C / Ctrl+Shift+C、選択があるとき）とペースト（Cmd+V / Ctrl+Shift+V）。
        let modifiers = &event.keystroke.modifiers;
        if modifiers.platform || (modifiers.control && modifiers.shift) {
            match event.keystroke.key.as_str() {
                "c" => {
                    if let Some(text) = self.selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        cx.stop_propagation();
                        return;
                    }
                }
                "v" => {
                    if let Some(item) = cx.read_from_clipboard() {
                        if let Some(text) = item.text() {
                            if let Some(session) = self.terminal() {
                                session.paste(&text);
                            }
                            cx.stop_propagation();
                            return;
                        }
                    }
                }
                _ => {}
            }
        }
        // Shift+PageUp / PageDown は履歴スクロールに使う（シェルへは送らない）。
        if event.keystroke.modifiers.shift {
            let page = match event.keystroke.key.as_str() {
                "pageup" => Some(1),
                "pagedown" => Some(-1),
                _ => None,
            };
            if let Some(direction) = page {
                if let Some(session) = self.terminal() {
                    let rows = session.rows as i32;
                    session.scroll(direction * rows);
                    cx.notify();
                }
                cx.stop_propagation();
                return;
            }
        }
        if let Some(bytes) = keystroke_to_bytes(
            &event.keystroke,
            self.terminal()
                .is_some_and(TerminalSession::application_cursor),
        ) {
            if let Some(session) = self.terminal() {
                session.write(&bytes);
            }
            // 入力したら選択は解除する。
            if self.term_selection.take().is_some() {
                cx.notify();
            }
            // Ctrl+S などアプリ側のキーバインドへは回さない（ターミナル入力を優先）。
            cx.stop_propagation();
        }
    }

    fn terminal_panel(&self, cx: &Context<Self>) -> Div {
        let title = self
            .sessions
            .get(self.active_session)
            .map(|tab| tab.title.clone())
            .unwrap_or_else(|| "ターミナル".to_string());
        let mut header = panel_header(title, Glyph::Terminal);
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

        if let Some(error) = &self.session_error {
            return panel.child(
                div()
                    .flex_1()
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .child(div().text_color(rgb(0xe9b4aa)).child(error.clone())),
            );
        }
        let Some(session) = self.terminal() else {
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
        // IME 入力ハンドラ登録用（canvas の paint で使う）。
        let input_focus = self.terminal_focus.clone();
        let input_entity = cx.entity();
        let body = div()
            .relative()
            .flex_1()
            .min_h_0()
            .bg(rgb(default_bg))
            .overflow_hidden()
            .track_focus(&self.terminal_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_terminal_key(event, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.terminal_focus.focus(window, cx);
                    let (col, row) = this.cell_from_position(event.position);
                    // Cmd+クリックはリンク（URL / ファイルパス）を開く。
                    if event.modifiers.platform && this.try_open_link(col, row, window, cx) {
                        return;
                    }
                    this.term_selection = Some((row, col, row, col));
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button == Some(MouseButton::Left) && this.term_selection.is_some()
                {
                    let (col, row) = this.cell_from_position(event.position);
                    if let Some(selection) = &mut this.term_selection {
                        selection.2 = row;
                        selection.3 = col;
                    }
                    cx.notify();
                }
            }))
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                let lines = match event.delta {
                    ScrollDelta::Lines(point) => point.y as i32,
                    ScrollDelta::Pixels(point) => (point.y / px(terminal::CELL_H)) as i32,
                };
                if let Some(session) = this.terminal() {
                    session.scroll(lines);
                }
                this.term_selection = None;
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
                                let origin = bounds.origin;
                                let _ = weak.update(cx, |this, cx| {
                                    this.terminal_resize(origin, cols, rows, cx)
                                });
                            }
                        },
                        move |bounds, _, window, cx| {
                            // ターミナルにフォーカスがある間、IME 入力を受け付ける。
                            window.handle_input(
                                &input_focus,
                                ElementInputHandler::new(bounds, input_entity.clone()),
                                cx,
                            );
                        },
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
                    .children(snapshot.lines.iter().enumerate().map(|(row_idx, line)| {
                        let selection = self.selection_range();
                        let mut col = 0usize;
                        div()
                            .h_flex()
                            .h(px(terminal::CELL_H))
                            .flex_shrink_0()
                            .children(line.runs.iter().map(|run| {
                                let selected = selection.is_some_and(|(sr, sc, er, ec)| {
                                    (row_idx, col + run.columns - 1) >= (sr, sc)
                                        && (row_idx, col) <= (er, ec)
                                });
                                col += run.columns;
                                let bg = if selected { SELECT_BG } else { run.bg };
                                let mut run_div = div()
                                    .w(px(terminal::CELL_W * run.columns as f32))
                                    .h(px(terminal::CELL_H))
                                    .flex_shrink_0()
                                    .whitespace_nowrap()
                                    .text_color(rgb(run.fg))
                                    .child(run.text.clone());
                                if bg != default_bg {
                                    run_div = run_div.bg(rgb(bg));
                                }
                                if run.bold {
                                    run_div = run_div.font_weight(FontWeight::BOLD);
                                }
                                run_div
                            }))
                    })),
            );
        let mut body = body;
        if let Some((col, row)) = snapshot.cursor {
            use alacritty_terminal::vte::ansi::CursorShape;
            let cursor = div()
                .absolute()
                .left(px(8. + col as f32 * terminal::CELL_W))
                .top(px(8. + row as f32 * terminal::CELL_H))
                .w(px(terminal::CELL_W))
                .h(px(terminal::CELL_H));
            body = match snapshot.cursor_shape {
                CursorShape::Underline => body.child(cursor.border_b_2().border_color(rgb(TEXT))),
                CursorShape::Beam => body.child(cursor.border_l_2().border_color(rgb(TEXT))),
                CursorShape::HollowBlock => body.child(cursor.border_1().border_color(rgb(TEXT))),
                _ => body,
            };
        }
        if !self.ime.text.is_empty() && self.terminal().map(TerminalSession::id) == self.ime.owner {
            if let Some((col, row)) = snapshot.cursor {
                // 変換中文字列をカーソル位置に重ねて出す（確定前は PTY へ送らない）。
                body = body.child(
                    div()
                        .absolute()
                        .left(px(8. + col as f32 * terminal::CELL_W))
                        .top(px(8. + row as f32 * terminal::CELL_H))
                        .font_family(MONO_FONT)
                        .text_size(px(12.5))
                        .bg(rgb(SELECT_BG))
                        .border_b_1()
                        .border_color(rgb(ACCENT))
                        .text_color(rgb(TEXT))
                        .child(self.ime.text.clone()),
                );
            }
        }
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
                        .on_click(cx.listener(|this, _, _, cx| this.open_project(cx))),
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
        header = header
            .child(div().flex_1())
            .child(
                Button::new("diff-mode-toggle")
                    .ghost()
                    .xsmall()
                    .label(if self.diff_split { "統合" } else { "分割" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.diff_split = !this.diff_split;
                        cx.notify();
                    })),
            )
            .child(
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
        let body = if self.diff_split {
            let parsed = diff::parse_unified_diff(&text);
            let rows = diff::to_split_rows(&parsed);
            div()
                .v_flex()
                .w_full()
                .py_2()
                .font_family(MONO_FONT)
                .text_size(px(12.))
                .children(rows.into_iter().map(|row| {
                    match row {
                        diff::SplitRow::Full(line) => {
                            let (fg, bg) = diff_kind_colors(line.kind);
                            let mut row_div = div()
                                .h_flex()
                                .h(px(17.))
                                .flex_shrink_0()
                                .px_3()
                                .whitespace_nowrap()
                                .text_color(rgb(fg))
                                .child(if line.text.is_empty() {
                                    " ".to_string()
                                } else {
                                    line.text.clone()
                                });
                            if let Some(bg) = bg {
                                row_div = row_div.bg(rgb(bg));
                            }
                            row_div
                        }
                        diff::SplitRow::Pair(old, new) => div()
                            .h_flex()
                            .h(px(17.))
                            .flex_shrink_0()
                            .w_full()
                            .child(split_cell(
                                old.as_ref(),
                                old.as_ref().and_then(|line| line.old_lineno),
                            ))
                            .child(div().w(px(1.)).flex_shrink_0().bg(rgb(LINE)))
                            .child(split_cell(
                                new.as_ref(),
                                new.as_ref().and_then(|line| line.new_lineno),
                            )),
                    }
                }))
        } else {
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
                }))
        };
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
                    .child(body),
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
/// UTF-16 コードユニット数（IME の範囲指定は UTF-16 基準）。
fn utf16_len(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

/// UTF-16 位置をバイト位置に変換する（文字の途中なら次の境界に寄せる）。
fn utf16_to_byte(s: &str, utf16_idx: usize) -> usize {
    let mut acc = 0usize;
    for (byte, ch) in s.char_indices() {
        if acc >= utf16_idx {
            return byte;
        }
        acc += ch.len_utf16();
    }
    s.len()
}

/// バイト位置を UTF-16 位置に変換する（バイト位置は文字境界前提）。
fn byte_to_utf16(s: &str, byte: usize) -> usize {
    let mut acc = 0usize;
    for (idx, ch) in s.char_indices() {
        if idx >= byte {
            break;
        }
        acc += ch.len_utf16();
    }
    acc
}

/// ターミナルの IME 入力。仮想ドキュメントは変換中文字列のみで、
/// 確定した文字列はそのまま PTY へ送る。
impl EntityInputHandler for Workbench {
    fn text_for_range(
        &mut self,
        range: std::ops::Range<usize>,
        adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let start = utf16_to_byte(&self.ime.text, range.start);
        let end = utf16_to_byte(&self.ime.text, range.end);
        if start > end {
            return None;
        }
        *adjusted_range =
            Some(byte_to_utf16(&self.ime.text, start)..byte_to_utf16(&self.ime.text, end));
        Some(self.ime.text[start..end].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: byte_to_utf16(&self.ime.text, self.ime.selection.0)
                ..byte_to_utf16(&self.ime.text, self.ime.selection.1),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        if self.ime.text.is_empty() {
            None
        } else {
            Some(0..utf16_len(&self.ime.text))
        }
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.ime.text.clone();
        self.commit_terminal_text(&text);
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_terminal_text(text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selection =
            new_selected_range.unwrap_or_else(|| utf16_len(new_text)..utf16_len(new_text));
        let selection = (
            utf16_to_byte(new_text, selection.start),
            utf16_to_byte(new_text, selection.end),
        );
        self.ime.mark(
            new_text.to_string(),
            selection,
            self.terminal().map(TerminalSession::id),
        );
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // 変換候補ウィンドウはターミナルカーソルの位置に出す。
        if let (Some(origin), Some(session)) = (self.term_origin, self.terminal()) {
            if let Some((col, row)) = session.snapshot().cursor {
                return Some(Bounds::new(
                    point(
                        origin.x + px(8. + col as f32 * terminal::CELL_W),
                        origin.y + px(8. + row as f32 * terminal::CELL_H),
                    ),
                    size(px(terminal::CELL_W), px(terminal::CELL_H)),
                ));
            }
        }
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

/// diff の行種別から unified 表示と同じ配色を返す（split 表示用）。
fn diff_kind_colors(kind: diff::DiffLineKind) -> (u32, Option<u32>) {
    match kind {
        diff::DiffLineKind::Added => (0x9ece8a, Some(0x22331d)),
        diff::DiffLineKind::Removed => (0xe08a7d, Some(0x38231f)),
        diff::DiffLineKind::HunkHeader => (ACCENT, None),
        diff::DiffLineKind::FileHeader | diff::DiffLineKind::Meta => (MUTED, None),
        diff::DiffLineKind::Context => (0xc6cdd8, None),
    }
}

/// split 表示の片側セル（行番号 + テキスト）。行が無い側は空セル。
fn split_cell(line: Option<&diff::DiffLine>, number: Option<usize>) -> Div {
    let mut cell = div().h_flex().flex_1().min_w_0().h(px(17.)).flex_shrink_0();
    cell = cell.child(
        div()
            .w(px(40.))
            .flex_shrink_0()
            .px_2()
            .text_color(rgb(MUTED))
            .child(number.map(|n| n.to_string()).unwrap_or_default()),
    );
    match line {
        Some(line) => {
            let (fg, bg) = diff_kind_colors(line.kind);
            cell = cell.text_color(rgb(fg));
            if let Some(bg) = bg {
                cell = cell.bg(rgb(bg));
            }
            cell.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(if line.text.is_empty() {
                        " ".to_string()
                    } else {
                        line.text.clone()
                    }),
            )
        }
        None => cell.bg(rgb(CANVAS)).child(div().flex_1()),
    }
}

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
            .on_action(cx.listener(|this, _: &Quit, window, cx| {
                if this.request_close(window, cx) {
                    cx.quit();
                }
            }))
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
                cx.listener(|this, _: &ShowSymbols, window, cx| this.show_symbols(window, cx)),
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
                    .children(if self.lsp_progress.is_empty() {
                        None
                    } else {
                        Some(
                            div()
                                .text_color(rgb(ACCENT))
                                .child(format!("LSP: {}", self.lsp_progress.join(" / "))),
                        )
                    })
                    .child(format!("開発版  {}", env!("CARGO_PKG_VERSION"))),
            )
    }
}

/// キー入力をターミナルへ送るバイト列に変換する。変換できないもの（Cmd 系など）は None。
fn keystroke_to_bytes(ks: &Keystroke, application_cursor: bool) -> Option<Vec<u8>> {
    let mods = &ks.modifiers;
    if mods.platform {
        return None;
    }
    // macOSの日本語入力ソース切替。Ctrl+J単独は改行として維持する。
    if cfg!(target_os = "macos")
        && mods.control
        && mods.shift
        && matches!(ks.key.as_str(), "j" | "J")
    {
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
        "up" => {
            if application_cursor {
                b"\x1bOA"
            } else {
                b"\x1b[A"
            }
        }
        "down" => {
            if application_cursor {
                b"\x1bOB"
            } else {
                b"\x1b[B"
            }
        }
        "right" => {
            if application_cursor {
                b"\x1bOC"
            } else {
                b"\x1b[C"
            }
        }
        "left" => {
            if application_cursor {
                b"\x1bOD"
            } else {
                b"\x1b[D"
            }
        }
        "home" => {
            if application_cursor {
                b"\x1bOH"
            } else {
                b"\x1b[H"
            }
        }
        "end" => {
            if application_cursor {
                b"\x1bOF"
            } else {
                b"\x1b[F"
            }
        }
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
            cx.set_menus(vec![Menu {
                name: "eren".into(),
                disabled: false,
                items: vec![MenuItem::action("終了", Quit)],
            }]);
            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-s", SaveFile, None),
                KeyBinding::new("ctrl-s", SaveFile, None),
                KeyBinding::new("cmd-e", ToggleEditor, None),
                KeyBinding::new("ctrl-e", ToggleEditor, None),
                KeyBinding::new("f12", GotoDefinition, None),
                KeyBinding::new("ctrl-f12", GotoImplementation, None),
                KeyBinding::new("shift-f12", FindReferences, None),
                KeyBinding::new("cmd-shift-o", ShowSymbols, None),
                KeyBinding::new("ctrl-shift-o", ShowSymbols, None),
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
                        let entity = cx.entity().downgrade();
                        window.on_window_should_close(cx, move |window, cx| {
                            entity
                                .update(cx, |this: &mut Workbench, cx| {
                                    this.request_close(window, cx)
                                })
                                .unwrap_or(true)
                        });
                        let mut workbench = Workbench::new(window, cx);
                        if let Some(root) = initial_project {
                            workbench.load_project(root);
                        } else {
                            workbench.restore_state(window, cx);
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
mod tests {
    use super::{byte_to_utf16, search_column, utf16_len, utf16_to_byte};

    #[test]
    fn terminal_keys_respect_application_cursor_and_control_shortcuts() {
        use super::{keystroke_to_bytes, Keystroke};
        for (key, normal, application) in [
            ("up", "\x1b[A", "\x1bOA"),
            ("down", "\x1b[B", "\x1bOB"),
            ("right", "\x1b[C", "\x1bOC"),
            ("left", "\x1b[D", "\x1bOD"),
            ("home", "\x1b[H", "\x1bOH"),
            ("end", "\x1b[F", "\x1bOF"),
        ] {
            let key = Keystroke::parse(key).unwrap();
            assert_eq!(keystroke_to_bytes(&key, false).unwrap(), normal.as_bytes());
            assert_eq!(
                keystroke_to_bytes(&key, true).unwrap(),
                application.as_bytes()
            );
        }
        for (key, bytes) in [
            ("ctrl-c", b"\x03".as_slice()),
            ("ctrl-j", b"\x0a"),
            ("alt-up", b"\x1b\x1bOA"),
            ("alt-x", b"\x1bx"),
        ] {
            let mut stroke = Keystroke::parse(key).unwrap();
            if key == "alt-x" {
                stroke.key_char = Some("x".into());
            }
            assert_eq!(keystroke_to_bytes(&stroke, true).unwrap(), bytes);
        }
        if cfg!(target_os = "macos") {
            assert_eq!(
                keystroke_to_bytes(&Keystroke::parse("ctrl-shift-j").unwrap(), false),
                None
            );
        }
    }

    #[test]
    fn search_maps_lowercase_expansion_to_original_column() {
        assert_eq!(search_column("İあ", "あ"), Some(1));
        assert_eq!(search_column("İab", "b"), Some(2));
        assert_eq!(search_column("İİあ", "あ"), Some(2));
        assert_eq!(search_column("İあ", "i"), Some(0));
        assert_eq!(search_column("İあ", "\u{307}"), Some(0));
        assert_eq!(search_column("İあ", "i\u{307}"), Some(0));
    }

    #[test]
    fn search_columns_count_original_characters() {
        for (line, query, column) in [
            ("Hello WORLD", "world", 6),
            ("日本語の検索", "検索", 4),
            ("a😀b", "b", 2),
            ("😀あ😀", "あ", 1),
            ("e\u{301}あ", "あ", 2),
            ("ΟΣ あ", "ος", 0),
            ("İΟΣ あ", "あ", 4),
        ] {
            assert_eq!(search_column(line, &query.to_lowercase()), Some(column));
        }
        assert_eq!(search_column("日本語", "missing"), None);
    }

    #[test]
    fn utf16_helpers_ascii() {
        assert_eq!(utf16_len("abc"), 3);
        assert_eq!(utf16_to_byte("abc", 0), 0);
        assert_eq!(utf16_to_byte("abc", 2), 2);
        assert_eq!(utf16_to_byte("abc", 99), 3);
        assert_eq!(byte_to_utf16("abc", 2), 2);
        assert_eq!(byte_to_utf16("abc", 3), 3);
    }

    #[test]
    fn utf16_helpers_japanese_and_emoji() {
        // 「あ」は UTF-16 で 1 単位、バイトでは 3。
        assert_eq!(utf16_len("あいう"), 3);
        assert_eq!(utf16_to_byte("あいう", 1), 3);
        assert_eq!(utf16_to_byte("あいう", 3), 9);
        assert_eq!(byte_to_utf16("あいう", 6), 2);
        // サロゲートペア（絵文字）は UTF-16 で 2 単位。
        assert_eq!(utf16_len("a😀b"), 4);
        assert_eq!(utf16_to_byte("a😀b", 3), 5);
        assert_eq!(byte_to_utf16("a😀b", 5), 3);
        // 往復変換。
        let s = "変換テスト😀x";
        for byte in (0..=s.len()).filter(|b| s.is_char_boundary(*b)) {
            assert_eq!(utf16_to_byte(s, byte_to_utf16(s, byte)), byte);
        }
    }
}

#[cfg(test)]
mod editor_safety_tests {
    use super::Workbench;
    use gpui_kit::{AppContext, TestAppContext, WindowOptions};

    #[gpui_kit::test]
    fn history_commits_only_after_confirmed_success(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = std::env::temp_dir().join(format!("eren-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        let cursor = gpui_kit::component::input::Position::new(0, 0);
        for outcome in ["save", "discard", "cancel", "failure"] {
            std::fs::write(&a, "a").unwrap();
            std::fs::write(&b, "b").unwrap();
            let (handle, entity) = cx.update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                    cx.new(|cx| Workbench::new(window, cx))
                })
                .unwrap()
            });
            cx.update_window(handle, |_, window, cx| {
                entity.update(cx, |this, cx| {
                    this.open_path(a.clone(), window, cx);
                    this.request_navigation(
                        b.clone(),
                        cursor,
                        Some(super::PendingNavigation::Jump(a.clone(), cursor)),
                        window,
                        cx,
                    );
                    assert_eq!(this.nav_back, vec![(a.clone(), cursor)]);
                    assert!(this.nav_forward.is_empty());
                    this.editor
                        .update(cx, |editor, cx| editor.set_value("edited b", window, cx));
                    this.dirty = true;
                    this.navigate_back(window, cx);
                    assert!(matches!(
                        this.pending_navigation,
                        Some(super::PendingNavigation::Back)
                    ));
                    assert_eq!(this.editing_path.as_ref(), Some(&b));
                    assert_eq!(this.nav_back, vec![(a.clone(), cursor)]);
                    assert!(this.nav_forward.is_empty());
                    match outcome {
                        "cancel" => {
                            this.cancel_pending();
                            assert!(this.pending_navigation.is_none());
                            assert_eq!(this.editing_path.as_ref(), Some(&b));
                            assert!(this.dirty);
                            assert_eq!(this.nav_back, vec![(a.clone(), cursor)]);
                            assert!(this.nav_forward.is_empty());
                            return;
                        }
                        "failure" => {
                            let permissions = std::fs::metadata(&b).unwrap().permissions();
                            let mut readonly = permissions.clone();
                            readonly.set_readonly(true);
                            std::fs::set_permissions(&b, readonly).unwrap();
                            this.save_and_open_pending(window, cx);
                            assert_eq!(this.editing_path.as_ref(), Some(&b));
                            assert!(this.dirty);
                            assert!(matches!(
                                this.pending_navigation,
                                Some(super::PendingNavigation::Back)
                            ));
                            assert_eq!(this.nav_back, vec![(a.clone(), cursor)]);
                            assert!(this.nav_forward.is_empty());
                            std::fs::set_permissions(&b, permissions).unwrap();
                            this.discard_and_open_pending(window, cx);
                        }
                        "save" => {
                            this.save_and_open_pending(window, cx);
                            assert_eq!(std::fs::read_to_string(&b).unwrap(), "edited b");
                        }
                        "discard" => this.discard_and_open_pending(window, cx),
                        _ => unreachable!(),
                    }
                    assert_eq!(this.editing_path.as_ref(), Some(&a));
                    assert!(this.nav_back.is_empty());
                    assert_eq!(this.nav_forward.len(), 1);
                    assert_eq!(this.nav_forward[0].0, b);
                    this.continue_pending(window, cx);
                    assert!(this.nav_back.is_empty());
                    assert_eq!(this.nav_forward.len(), 1);
                    this.dirty = true;
                    this.navigate_forward(window, cx);
                    assert!(matches!(
                        this.pending_navigation,
                        Some(super::PendingNavigation::Forward)
                    ));
                    assert_eq!(this.nav_forward.len(), 1);
                    assert!(this.nav_back.is_empty());
                    this.discard_and_open_pending(window, cx);
                    assert_eq!(this.editing_path.as_ref(), Some(&b));
                    assert!(this.nav_forward.is_empty());
                    assert_eq!(this.nav_back.len(), 1);
                    assert_eq!(this.nav_back[0].0, a);
                    // Failed reads also leave both histories intact, even after discard.
                    this.nav_back.push((dir.join("missing"), cursor));
                    let back = this.nav_back.clone();
                    let forward = this.nav_forward.clone();
                    this.dirty = true;
                    this.navigate_back(window, cx);
                    this.discard_and_open_pending(window, cx);
                    assert_eq!(this.nav_back, back);
                    assert_eq!(this.nav_forward, forward);
                    assert_eq!(this.editing_path.as_ref(), Some(&b));
                    assert!(this.dirty);
                    // A new jump must not alter either history before confirmation.
                    this.request_navigation(
                        a.clone(),
                        cursor,
                        Some(super::PendingNavigation::Jump(b.clone(), cursor)),
                        window,
                        cx,
                    );
                    assert_eq!(this.nav_back, back);
                    assert_eq!(this.nav_forward, forward);
                    this.cancel_pending();
                    assert_eq!(this.nav_back, back);
                    assert_eq!(this.nav_forward, forward);
                })
            })
            .unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[gpui_kit::test]
    fn layout_survives_repeated_restore_and_failed_save(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = std::env::temp_dir().join(format!("eren-layout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("state.json");
        let file = dir.join("file.txt");
        std::fs::write(&file, "document").unwrap();
        for editing_file in [None, Some(file.clone())] {
            let expected = serde_json::json!({"project": dir, "sessions": ["shell", "shell"],
                "active_session": 1, "editing_file": editing_file, "editor_open": false});
            std::fs::write(&state_path, expected.to_string()).unwrap();
            for _ in 0..2 {
                let (handle, entity) = cx.update(|cx| {
                    gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                        cx.new(|cx| Workbench::new(window, cx))
                    })
                    .unwrap()
                });
                cx.update_window(handle, |_, window, cx| {
                    entity.update(cx, |this, cx| {
                        this.state_path = Some(state_path.clone());
                        this.restore_state(window, cx);
                        assert_eq!(this.sessions.len(), 2, "{:?}", this.session_error);
                        assert_eq!(this.active_session, 1);
                        assert_eq!(this.editing_path, editing_file);
                        assert!(!this.editor_open);
                        let saved: serde_json::Value =
                            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
                        assert_eq!(saved, expected);
                        // Intermediate persistence must not replace the complete state.
                        this.restoring_state = true;
                        this.active_session = 0;
                        this.save_state();
                        assert_eq!(
                            serde_json::from_slice::<serde_json::Value>(
                                &std::fs::read(&state_path).unwrap()
                            )
                            .unwrap(),
                            expected
                        );
                        this.restoring_state = false;
                        this.active_session = 1;
                        let permissions = std::fs::metadata(&state_path).unwrap().permissions();
                        let mut readonly = permissions.clone();
                        readonly.set_readonly(true);
                        std::fs::set_permissions(&state_path, readonly).unwrap();
                        this.active_session = 0;
                        this.save_state();
                        assert!(this
                            .status_message
                            .as_ref()
                            .unwrap()
                            .contains("状態を保存できなかった"));
                        assert_eq!(
                            serde_json::from_slice::<serde_json::Value>(
                                &std::fs::read(&state_path).unwrap()
                            )
                            .unwrap(),
                            expected
                        );
                        std::fs::set_permissions(&state_path, permissions).unwrap();
                        for tab in this.sessions.drain(..) {
                            tab.session.shutdown();
                        }
                        window.remove_window();
                    })
                })
                .unwrap();
            }
        }
        std::fs::write(&state_path, "{invalid").unwrap();
        let (handle, entity) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| Workbench::new(window, cx))
            })
            .unwrap()
        });
        cx.update_window(handle, |_, window, cx| {
            entity.update(cx, |this, cx| {
                this.state_path = Some(state_path.clone());
                this.restore_state(window, cx);
                assert!(this.project.is_none());
                assert_eq!(std::fs::read_to_string(&state_path).unwrap(), "{invalid");
            })
        })
        .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[gpui_kit::test]
    fn pending_file_navigation_reveals_confirmation(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = std::env::temp_dir().join(format!("eren-pending-visible-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let (handle, entity) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| Workbench::new(window, cx))
            })
            .unwrap()
        });
        cx.update_window(handle, |_, window, cx| {
            entity.update(cx, |this, cx| {
                for search in [false, true] {
                    for diff in [false, true] {
                        this.open_path(a.clone(), window, cx);
                        this.editor
                            .update(cx, |editor, cx| editor.set_value("unsaved", window, cx));
                        this.dirty = true;
                        this.editor_open = diff;
                        this.diff_target = diff.then(|| "a.txt".into());
                        this.diff_text = diff.then(|| "diff".into());
                        if search {
                            this.search_results = vec![super::SearchMatch {
                                path: b.clone(),
                                rel: "b.txt".into(),
                                line: 0,
                                col: 0,
                                text: "b".into(),
                            }];
                            this.open_search_match(0, window, cx);
                        } else {
                            this.on_tree_click(b.clone(), false, window, cx);
                        }
                        assert_eq!(this.pending_path.as_ref(), Some(&b));
                        assert!(
                            this.editor_open,
                            "search={search}, diff={diff}: 確認バーが非表示"
                        );
                        assert!(
                            this.diff_target.is_none(),
                            "search={search}, diff={diff}: diff が確認バーを隠す"
                        );
                        assert!(this.diff_text.is_none());
                        assert_eq!(this.editing_path.as_ref(), Some(&a));
                        assert!(this.dirty);
                        assert_eq!(this.editor.read(cx).text().to_string(), "unsaved");
                        this.cancel_pending();
                        assert!(this.pending_path.is_none());
                        assert_eq!(this.editor.read(cx).text().to_string(), "unsaved");
                    }
                }
            })
        })
        .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[gpui_kit::test]
    fn failed_navigation_and_save_keep_the_document(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (handle, entity) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| Workbench::new(window, cx))
            })
            .unwrap()
        });
        let dir = std::env::temp_dir().join(format!("eren-editor-safety-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("file.txt");
        std::fs::write(&path, "日本語😀\r\n").unwrap();
        cx.update_window(handle, |_, window, cx| {
            entity.update(cx, |this, cx| {
                this.open_path(path.clone(), window, cx);
                assert_eq!(this.editor.read(cx).text().to_string(), "日本語😀\r\n");
                this.editor
                    .update(cx, |editor, cx| editor.set_value("unsaved", window, cx));
                this.dirty = true;
                this.open_path(dir.join("missing"), window, cx);
                assert_eq!(this.editing_path.as_ref(), Some(&path));
                assert!(this.dirty);
                assert_eq!(this.editor.read(cx).text().to_string(), "unsaved");
                this.request_project(dir.join("other"), cx);
                assert_eq!(this.pending_project, Some(dir.join("other")));
                assert!(this.project.is_none());
                assert!(!this.request_close(window, cx));
                assert!(this.pending_close);
                assert_eq!(this.editor.read(cx).text().to_string(), "unsaved");
                this.cancel_pending();
                assert!(!this.pending_close);
                assert!(this.pending_project.is_none());
                assert!(this.dirty);
                this.jump_to(
                    path.clone(),
                    gpui_kit::component::input::Position::new(0, 0),
                    window,
                    cx,
                );
                assert_eq!(this.editor.read(cx).text().to_string(), "unsaved");
                this.pending_path = Some(dir.join("missing"));
                let permissions = std::fs::metadata(&path).unwrap().permissions();
                let mut readonly = permissions.clone();
                readonly.set_readonly(true);
                std::fs::set_permissions(&path, readonly).unwrap();
                this.save_and_open_pending(window, cx);
                assert!(this.dirty);
                assert!(this.pending_path.is_some());
                assert_eq!(this.editor.read(cx).text().to_string(), "unsaved");
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "日本語😀\r\n");
                std::fs::set_permissions(&path, permissions).unwrap();
                std::fs::write(&path, "external").unwrap();
                this.save_and_open_pending(window, cx);
                assert!(this.save_conflict);
                assert!(this.dirty);
                assert!(this.pending_path.is_some());
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "external");
                assert!(!this.save_current(cx));
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "external");
                this.discard_and_open_pending(window, cx);
                assert!(this.dirty);
                assert_eq!(this.editing_path.as_ref(), Some(&path));
                assert_eq!(this.editor.read(cx).text().to_string(), "unsaved");
                // Invalid external data must never replace the editable document.
                std::fs::write(&path, [255]).unwrap();
                this.reload_current_file(window, cx);
                assert!(this.dirty);
                assert_eq!(this.editor.read(cx).text().to_string(), "unsaved");
                assert!(this.write_current(&path, "unsaved".into(), cx));
                assert!(!this.dirty);
                assert!(!this.save_conflict);
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "unsaved");
                this.dirty = true;
                std::fs::remove_file(&path).unwrap();
                assert!(!this.save_current(cx));
                assert!(this.save_conflict);
                assert!(this.dirty);
                assert!(!path.exists());
                this.cancel_pending();
                assert!(this.dirty);
                std::fs::write(&path, "reload 日本語😀\r\n").unwrap();
                this.reload_current_file(window, cx);
                assert!(!this.dirty);
                assert_eq!(
                    this.editor.read(cx).text().to_string(),
                    "reload 日本語😀\r\n"
                );
            })
        })
        .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
