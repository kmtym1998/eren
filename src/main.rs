use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::button::*;
use gpui_kit::component::{Icon, Sizable, StyledExt, Theme, ThemeMode, TitleBar};
use gpui_kit::*;

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

/// 開いているファイルの中身（読み取り専用ビューア用）。
struct OpenFile {
    path: PathBuf,
    lines: Vec<String>,
    truncated: bool,
}

const MAX_LINES: usize = 5000;
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
    open_file: Option<OpenFile>,
    file_error: Option<String>,
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
    fn new() -> Self {
        Self {
            project: None,
            picker_error: None,
            tree: Vec::new(),
            expanded: HashSet::new(),
            branch: None,
            open_file: None,
            file_error: None,
        }
    }

    fn load_project(&mut self, root: PathBuf) {
        self.tree = read_dir_entries(&root);
        self.branch = git_branch(&root);
        self.expanded.clear();
        self.open_file = None;
        self.file_error = None;
        self.project = Some(root);
    }

    fn on_tree_click(&mut self, path: PathBuf, is_dir: bool, cx: &mut Context<Self>) {
        if is_dir {
            if self.expanded.contains(&path) {
                self.expanded.remove(&path);
            } else {
                ensure_children(&mut self.tree, &path);
                self.expanded.insert(path);
            }
        } else {
            self.open_path(path);
        }
        cx.notify();
    }

    fn open_path(&mut self, path: PathBuf) {
        self.file_error = None;
        match std::fs::metadata(&path) {
            Ok(meta) if meta.len() > MAX_FILE_BYTES => {
                self.file_error = Some("ファイルが大きいため表示できない（上限 2 MB）。".into());
                self.open_file = None;
                return;
            }
            Ok(_) => {}
            Err(_) => {
                self.file_error = Some("ファイルを開けなかった。".into());
                self.open_file = None;
                return;
            }
        }
        match std::fs::read(&path) {
            Ok(bytes) => {
                if bytes.iter().take(8192).any(|b| *b == 0) {
                    self.file_error = Some("バイナリファイルは表示できない。".into());
                    self.open_file = None;
                    return;
                }
                let text = String::from_utf8_lossy(&bytes);
                let mut lines: Vec<String> = text
                    .lines()
                    .take(MAX_LINES + 1)
                    .map(|line| line.replace('\t', "    "))
                    .collect();
                let truncated = lines.len() > MAX_LINES;
                lines.truncate(MAX_LINES);
                self.open_file = Some(OpenFile {
                    path,
                    lines,
                    truncated,
                });
            }
            Err(_) => {
                self.file_error = Some("ファイルを読み込めなかった。".into());
                self.open_file = None;
            }
        }
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
                            this.load_project(root);
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
                            .on_click(cx.listener(|this, _, _, cx| this.open_project(cx))),
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
                .open_file
                .as_ref()
                .is_some_and(|file| file.path == row.path);
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
            list = list.child(item.on_click(cx.listener(move |this, _, _, cx| {
                this.on_tree_click(path.clone(), is_dir, cx);
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

        div()
            .v_flex()
            .w(px(264.))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(header)
            .child(list)
    }

    fn viewer(&self) -> Div {
        let mut panel = div().v_flex().flex_1().min_w_0().h_full();

        match (&self.open_file, &self.file_error) {
            (Some(file), _) => {
                let title = file
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let mut code = div()
                    .id("code-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .overflow_x_scroll()
                    .py_3()
                    .font_family("Menlo")
                    .text_size(px(12.));
                for (index, line) in file.lines.iter().enumerate() {
                    code = code.child(
                        div()
                            .h_flex()
                            .flex_none()
                            .child(
                                div()
                                    .w(px(52.))
                                    .flex_shrink_0()
                                    .pr_3()
                                    .text_color(rgb(0x5b6575))
                                    .child(format!("{}", index + 1)),
                            )
                            .child(div().whitespace_nowrap().child(if line.is_empty() {
                                " ".to_string()
                            } else {
                                line.clone()
                            })),
                    );
                }
                if file.truncated {
                    code = code.child(
                        div()
                            .px_4()
                            .py_2()
                            .text_color(rgb(MUTED))
                            .child(format!("… 先頭 {} 行のみ表示", MAX_LINES)),
                    );
                }
                panel = panel.child(panel_header(title, Glyph::File)).child(code);
            }
            (None, Some(error)) => {
                panel = panel.child(panel_header("ビューア", Glyph::File)).child(
                    div()
                        .flex_1()
                        .v_flex()
                        .items_center()
                        .justify_center()
                        .child(div().text_color(rgb(0xe9b4aa)).child(error.clone())),
                );
            }
            (None, None) => {
                panel = panel.child(panel_header("ビューア", Glyph::File)).child(
                    div()
                        .flex_1()
                        .v_flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .text_color(rgb(MUTED))
                                .child("左のツリーでファイルを選ぶと、ここに表示する"),
                        ),
                );
            }
        }
        panel
    }

    fn workspace(&self, cx: &Context<Self>) -> Div {
        if self.project.is_some() {
            return div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(panel_header("作業領域", Glyph::CodeXml))
                .child(
                    div()
                        .h_flex()
                        .flex_1()
                        .min_h_0()
                        .child(self.tree_panel(cx))
                        .child(self.viewer()),
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
                        .child("読み取り専用ビューア（編集はできない）"),
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

    fn changes(&self) -> Div {
        div()
            .v_flex()
            .w(px(252.))
            .flex_shrink_0()
            .h_full()
            .border_l_1()
            .border_color(rgb(LINE))
            .child(panel_header("変更", Glyph::GitCompareArrows))
            .child(
                div()
                    .px_5()
                    .pt_8()
                    .v_flex()
                    .gap_3()
                    .child(icon(Glyph::GitBranch).text_color(rgb(MUTED)))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child("差分の表示を待機中"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(MUTED))
                            .child("変更ファイルとコードの差分を、ここで確認できるようになる。"),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .mx_4()
                    .mb_5()
                    .pt_4()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child("Git 連携は準備中"),
            )
    }
}

impl Render for Workbench {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .size_full()
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
                    .child(self.changes()),
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
                move |_, cx| {
                    cx.new(|_| {
                        let mut workbench = Workbench::new();
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
