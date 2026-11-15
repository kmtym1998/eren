use std::path::PathBuf;

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

pub struct Workbench {
    project: Option<PathBuf>,
    picker_error: Option<String>,
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

fn panel_header(name: &'static str, glyph: Glyph) -> Div {
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
        .child(div().text_color(rgb(TEXT)).child(name))
}

impl Workbench {
    fn new() -> Self {
        Self {
            project: None,
            picker_error: None,
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
                        this.project = paths.into_iter().next();
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

    fn workspace(&self, cx: &Context<Self>) -> Div {
        let title = if self.project.is_some() {
            "プロジェクトを開いた"
        } else {
            "ここから、作業を始めよう"
        };
        let detail = match &self.project {
            Some(path) => path.to_string_lossy().into_owned(),
            None => "ローカルのプロジェクトを、このワークスペースに。".into(),
        };
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
                        .label(if self.project.is_some() {
                            "別のフォルダを開く"
                        } else {
                            "プロジェクトを開く"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.open_project(cx))),
                ),
            );
        if let Some(error) = &self.picker_error {
            empty = empty.child(div().text_color(rgb(0xe9b4aa)).child(error.clone()));
        }
        if self.project.is_some() {
            empty = empty.child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child("ファイルビューアと Git 差分は、これから追加する。"),
            );
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
        println!("使い方: eren [--version|--help]");
        println!("引数なしで GUI を起動する。");
        return;
    }

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
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
                |_, cx| cx.new(|_| Workbench::new()),
            )
            .expect("ウィンドウを開けなかった");
            cx.activate(true);
        });
}
