use gpui_kit::component::button::*;
use gpui_kit::component::label::Label;
use gpui_kit::component::*;
use gpui_kit::*;

/// First skeleton of a Sirio/Herdr-like agent workspace.
///
/// Layout only for now:
/// - Left: project / worktree sidebar with agent status rows
/// - Center: viewer area (file tree + code viewer + diff will live here)
/// - Right: changes / session placeholder
///
/// The real pieces (gpui-component Tree, Editor with LSP providers,
/// Diff view, terminal pane) are wired in step by step; see SPEC.md.
pub struct Workbench {
    project_name: String,
    agents: Vec<(String, String)>,
}

impl Workbench {
    fn new() -> Self {
        Self {
            project_name: "eren (no project open yet)".to_string(),
            agents: vec![
                ("claude-code".to_string(), "🟢 running".to_string()),
                ("codex".to_string(), "🟡 waiting for input".to_string()),
                ("opencode".to_string(), "⚪ idle".to_string()),
            ],
        }
    }
}

impl Render for Workbench {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut sidebar = div().v_flex().gap_2().w(px(280.)).child(Label::new("Project"));
        sidebar = sidebar.child(Label::new(self.project_name.clone()));
        sidebar = sidebar.child(Label::new("Agents"));
        for (name, status) in &self.agents {
            sidebar = sidebar.child(Label::new(format!("{status}  {name}")));
        }
        sidebar = sidebar.child(
            Button::new("open-project")
                .label("Open project…")
                .on_click(|_, _, _| println!("TODO: project picker")),
        );

        let center = div()
            .v_flex()
            .gap_2()
            .flex_1()
            .child(Label::new("Viewer"))
            .child(Label::new(
                "File tree, read-only code viewer (tree-sitter), git diff and LSP jumps \
                 (definition / implementation / references) will be wired here.",
            ));

        let right = div()
            .v_flex()
            .gap_2()
            .w(px(280.))
            .child(Label::new("Changes"))
            .child(Label::new("Changed files and diffs will appear here."));

        div().h_flex().gap_4().size_full().child(sidebar).child(center).child(right)
    }
}

fn main() {
    // CLI flags that must work without a display (Homebrew's formula test,
    // `eren --version` in scripts, etc.). Anything else launches the GUI.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("eren {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("eren {} — a native workspace for coding agents", env!("CARGO_PKG_VERSION"));
        println!("Usage: eren [--version|--help]");
        println!("With no arguments, opens the GUI.");
        return;
    }

    gpui_kit::application().run(move |cx| {
        // This must be called before using any GPUI Component features.
        gpui_kit::init(cx);

        // Opens a window with a `Root` wrapping the view, so dialogs, sheets,
        // notifications and menus work in it.
        gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
            cx.new(|_| Workbench::new())
        })
        .expect("Failed to open window");
    });
}
