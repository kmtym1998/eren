//! alacritty_terminal を使った PTY ターミナル。
//!
//! シェルを PTY で起動し、VT パースとグリッド管理は alacritty_terminal に任せる。
//! 描画は Workbench 側で、ここで作るスナップショット（行ごとのスタイル付き run）を表示する。

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::Term;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::tty::{self, Options as TtyOptions, Shell};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};

/// 1 セルの想定サイズ（px）。12.5px の等幅フォント前提。
pub const CELL_W: f32 = 7.6;
pub const CELL_H: f32 = 17.0;

/// 既定の前景・背景（パレットに無いときのフォールバック。UI の TEXT / CANVAS と合わせる）。
const DEFAULT_FG: Rgb = Rgb {
    r: 0xe2,
    g: 0xe7,
    b: 0xef,
};
const DEFAULT_BG: Rgb = Rgb {
    r: 0x1b,
    g: 0x1e,
    b: 0x24,
};

#[derive(Clone)]
struct WakeListener {
    wake: Arc<AtomicBool>,
    events: mpsc::Sender<Event>,
}

impl EventListener for WakeListener {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => self.wake.store(true, Ordering::SeqCst),
            Event::PtyWrite(_) | Event::ColorRequest(..) | Event::TextAreaSizeRequest(_) => {
                // パーサーは Term のロック中に呼ぶため、ここでは問い合わせを積むだけにする。
                let _ = self.events.send(event);
                self.wake.store(true, Ordering::SeqCst);
            }
            _ => {}
        }
    }
}

/// 保持するスクロールバック行数。
const SCROLLBACK: usize = 10_000;

struct TermSize {
    cols: usize,
    lines: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.lines + SCROLLBACK
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// スタイル付きの文字列の固まり。
#[derive(Clone, PartialEq)]
pub struct Run {
    pub text: String,
    pub fg: u32,
    pub bg: u32,
    pub bold: bool,
    /// 全角文字や結合文字を含めた端末上の占有セル数。
    pub columns: usize,
}

/// 1 行分の run 列。
pub struct Line {
    pub runs: Vec<Run>,
}

/// 画面全体のスナップショット。default_bg は「背景色を塗らなくてよい色」。
pub struct Snapshot {
    pub lines: Vec<Line>,
    pub default_bg: u32,
    /// 履歴を遡っている行数（0 = 最下部）。
    pub display_offset: usize,
}

pub struct TerminalSession {
    term: Arc<FairMutex<Term<WakeListener>>>,
    sender: EventLoopSender,
    events: mpsc::Receiver<Event>,
    pub wake: Arc<AtomicBool>,
    pub cols: usize,
    pub rows: usize,
}

fn rgb_u32(c: Rgb) -> u32 {
    ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32
}

impl TerminalSession {
    /// `cwd` でログインシェルを起動する。
    pub fn new(cwd: &Path, cols: usize, rows: usize) -> std::io::Result<Self> {
        let wake = Arc::new(AtomicBool::new(true));
        let (events_tx, events) = mpsc::channel();
        let listener = WakeListener {
            wake: wake.clone(),
            events: events_tx,
        };
        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &TermSize { cols, lines: rows },
            listener.clone(),
        )));
        let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let options = TtyOptions {
            shell: Some(Shell::new(shell_path, vec!["-l".to_string()])),
            working_directory: Some(cwd.to_path_buf()),
            drain_on_exit: false,
            env: HashMap::new(),
        };
        let window_size = WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: CELL_W as u16,
            cell_height: CELL_H as u16,
        };
        let pty = tty::new(&options, window_size, 0)?;
        let event_loop = EventLoop::new(term.clone(), listener, pty, false, false)?;
        let sender = event_loop.channel();
        let _handle = event_loop.spawn();
        Ok(Self {
            term,
            sender,
            events,
            wake,
            cols,
            rows,
        })
    }

    pub fn write(&self, bytes: &[u8]) {
        // 入力時は最下部へ戻す（一般的なターミナルの挙動）。
        self.term.lock().scroll_display(Scroll::Bottom);
        let _ = self.sender.send(Msg::Input(Cow::Owned(bytes.to_vec())));
    }

    /// 履歴スクロール。正で過去方向、負で最新方向へ。
    pub fn scroll(&self, delta_lines: i32) {
        if delta_lines != 0 {
            self.term.lock().scroll_display(Scroll::Delta(delta_lines));
            self.wake.store(true, Ordering::SeqCst);
        }
    }

    pub fn shutdown(&self) {
        let _ = self.sender.send(Msg::Shutdown);
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(2);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.lock().resize(TermSize { cols, lines: rows });
        let _ = self.sender.send(Msg::Resize(WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: CELL_W as u16,
            cell_height: CELL_H as u16,
        }));
    }

    /// PTY スレッドのパースが終わってから応答を作る。ユーザー入力と違い履歴位置は変えない。
    pub fn process_events(&self) {
        for event in self.events.try_iter() {
            let reply =
                reply_to_event(event, &*self.term.lock(), window_size(self.cols, self.rows));
            if let Some(reply) = reply {
                let _ = self.sender.send(Msg::Input(Cow::Owned(reply.into_bytes())));
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        snapshot(&*self.term.lock(), self.cols, self.rows)
    }
}

fn window_size(cols: usize, rows: usize) -> WindowSize {
    WindowSize {
        num_lines: rows as u16,
        num_cols: cols as u16,
        cell_width: CELL_W as u16,
        cell_height: CELL_H as u16,
    }
}

/// 描画と問い合わせ応答で同じパレットを使い、OSC による上書きも優先する。
fn palette_color(colors: &Colors, index: usize) -> Rgb {
    if index < alacritty_terminal::term::color::COUNT {
        if let Some(color) = colors[index] {
            return color;
        }
    }
    const ANSI: [u32; 16] = [
        0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x7f7f7f,
        0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
    ];
    let from_hex = |hex: u32| Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    };
    match index {
        0..=15 => from_hex(ANSI[index]),
        16..=231 => {
            let n = index - 16;
            let component = |value: usize| {
                if value == 0 {
                    0
                } else {
                    (55 + value * 40) as u8
                }
            };
            Rgb {
                r: component(n / 36),
                g: component(n / 6 % 6),
                b: component(n % 6),
            }
        }
        232..=255 => {
            let level = (8 + (index - 232) * 10) as u8;
            Rgb {
                r: level,
                g: level,
                b: level,
            }
        }
        257 => DEFAULT_BG,
        259..=266 => palette_color(colors, index - 259) * 0.66,
        268 => palette_color(colors, NamedColor::Foreground as usize) * 0.66,
        _ => DEFAULT_FG,
    }
}

fn reply_to_event<T: EventListener>(
    event: Event,
    term: &Term<T>,
    size: WindowSize,
) -> Option<String> {
    match event {
        Event::PtyWrite(text) => Some(text),
        Event::ColorRequest(index, format) => Some(format(palette_color(term.colors(), index))),
        Event::TextAreaSizeRequest(format) => Some(format(size)),
        _ => None,
    }
}

fn snapshot<T: EventListener>(term: &Term<T>, cols: usize, rows: usize) -> Snapshot {
    let content = term.renderable_content();
    let colors = content.colors;
    let default_fg = palette_color(colors, NamedColor::Foreground as usize);
    let default_bg = palette_color(colors, NamedColor::Background as usize);
    let show_cursor = content.mode.contains(TermMode::SHOW_CURSOR)
        && content.cursor.shape == CursorShape::Block
        && content.display_offset == 0;
    let resolve = |color: Color| match color {
        Color::Spec(rgb) => rgb,
        Color::Indexed(index) => palette_color(colors, index as usize),
        Color::Named(name) => palette_color(colors, name as usize),
    };
    let blank = Run {
        text: " ".into(),
        fg: rgb_u32(default_fg),
        bg: rgb_u32(default_bg),
        bold: false,
        columns: 1,
    };
    let mut grid = vec![vec![Some(blank); cols]; rows];
    for indexed in content.display_iter {
        // 履歴の先頭は負の行番号。描画する viewport の行へ戻す。
        let line = indexed.point.line.0 + content.display_offset as i32;
        let col = indexed.point.column.0;
        if line < 0 || line as usize >= rows || col >= cols {
            continue;
        }
        let cell = indexed.cell;
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            grid[line as usize][col] = None;
            continue;
        }
        if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let mut fg = resolve(cell.fg);
        let mut bg = resolve(cell.bg);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if show_cursor && indexed.point == content.cursor.point {
            std::mem::swap(&mut fg, &mut bg);
        }
        let columns = if cell.flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        let text = if cell.flags.contains(Flags::HIDDEN) || cell.c == '\0' {
            " ".repeat(columns)
        } else {
            let mut text = cell.c.to_string();
            if let Some(chars) = cell.zerowidth() {
                text.extend(chars);
            }
            text
        };
        grid[line as usize][col] = Some(Run {
            text,
            fg: rgb_u32(fg),
            bg: rgb_u32(bg),
            bold: cell.flags.contains(Flags::BOLD),
            columns,
        });
    }
    let default_bg = rgb_u32(default_bg);
    let lines =
        grid.into_iter()
            .map(|cells| {
                let mut cells: Vec<Run> = cells.into_iter().flatten().collect();
                while cells.last().is_some_and(|cell| {
                    cell.text.trim_matches(' ').is_empty() && cell.bg == default_bg
                }) {
                    cells.pop();
                }
                let mut runs: Vec<Run> = Vec::new();
                for cell in cells {
                    match runs.last_mut() {
                        Some(run)
                            if run.fg == cell.fg && run.bg == cell.bg && run.bold == cell.bold =>
                        {
                            run.text.push_str(&cell.text);
                            run.columns += cell.columns;
                        }
                        _ => runs.push(cell),
                    }
                }
                Line { runs }
            })
            .collect();
    Snapshot {
        lines,
        default_bg,
        display_offset: content.display_offset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::Processor;

    fn parsed(
        bytes: &[u8],
        cols: usize,
        rows: usize,
    ) -> (Term<WakeListener>, mpsc::Receiver<Event>) {
        let (events, rx) = mpsc::channel();
        let listener = WakeListener {
            wake: Arc::new(AtomicBool::new(false)),
            events,
        };
        let mut term = Term::new(Config::default(), &TermSize { cols, lines: rows }, listener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b[?25l");
        parser.advance(&mut term, bytes);
        (term, rx)
    }

    fn line_text(line: &Line) -> String {
        line.runs.iter().map(|run| run.text.as_str()).collect()
    }

    #[test]
    fn renders_ansi_indexed_and_truecolor_with_distinct_colors() {
        let (term, _) = parsed(b"\x1b[31mR\x1b[38;5;46mG\x1b[38;2;1;2;3mT\x1b[0mN", 20, 2);
        let view = snapshot(&term, 20, 2);
        let runs = &view.lines[0].runs;
        assert_eq!(line_text(&view.lines[0]), "RGTN");
        assert_eq!(
            runs.iter().map(|run| run.fg).collect::<Vec<_>>(),
            [0xcd0000, 0x00ff00, 0x010203, rgb_u32(DEFAULT_FG)]
        );
    }

    #[test]
    fn honors_osc_palette_override_and_reset() {
        let (mut term, _) = parsed(b"\x1b]4;1;rgb:12/34/56\x07\x1b[31mR", 10, 2);
        assert_eq!(snapshot(&term, 10, 2).lines[0].runs[0].fg, 0x123456);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b]104;1\x07");
        assert_eq!(snapshot(&term, 10, 2).lines[0].runs[0].fg, 0xcd0000);
    }

    #[test]
    fn covers_color_cube_grayscale_and_ansi_background() {
        let colors = Colors::default();
        assert_eq!(rgb_u32(palette_color(&colors, 16)), 0);
        assert_eq!(rgb_u32(palette_color(&colors, 231)), 0xffffff);
        assert_eq!(rgb_u32(palette_color(&colors, 232)), 0x080808);
        assert_eq!(rgb_u32(palette_color(&colors, 255)), 0xeeeeee);
        let (term, _) = parsed(b"\x1b[44mB", 10, 2);
        assert_eq!(snapshot(&term, 10, 2).lines[0].runs[0].bg, 0x0000ee);
    }

    #[test]
    fn japanese_and_combining_characters_keep_text_and_cell_width() {
        let (term, _) = parsed("日本語|e\u{301}A".as_bytes(), 20, 2);
        let view = snapshot(&term, 20, 2);
        assert_eq!(line_text(&view.lines[0]), "日本語|e\u{301}A");
        assert_eq!(
            view.lines[0]
                .runs
                .iter()
                .map(|run| run.columns)
                .sum::<usize>(),
            9
        );
    }

    #[test]
    fn wide_characters_wrap_without_extra_spaces() {
        let (term, _) = parsed("abc日本".as_bytes(), 4, 3);
        let view = snapshot(&term, 4, 3);
        assert_eq!(line_text(&view.lines[0]), "abc");
        assert_eq!(line_text(&view.lines[1]), "日本");
        assert_eq!(
            view.lines[1]
                .runs
                .iter()
                .map(|run| run.columns)
                .sum::<usize>(),
            4
        );
    }

    #[test]
    fn responds_to_cursor_status_color_and_window_size_queries() {
        let (term, events) = parsed(b"abc\x1b[6n\x1b[5n\x1b]11;?\x07\x1b[18t", 80, 24);
        let replies: Vec<_> = events
            .try_iter()
            .filter_map(|event| reply_to_event(event, &term, window_size(80, 24)))
            .collect();
        assert_eq!(
            replies,
            [
                "\x1b[1;4R",
                "\x1b[0n",
                "\x1b]11;rgb:1b1b/1e1e/2424\x07",
                "\x1b[8;24;80t"
            ]
        );
    }

    #[test]
    fn queries_use_osc_overrides() {
        let (term, events) = parsed(b"\x1b]4;1;rgb:12/34/56\x07\x1b]4;1;?\x07", 20, 2);
        let replies: Vec<_> = events
            .try_iter()
            .filter_map(|event| reply_to_event(event, &term, window_size(20, 2)))
            .collect();
        assert_eq!(replies, ["\x1b]4;1;rgb:1212/3434/5656\x07"]);
    }

    #[test]
    fn scrollback_maps_negative_grid_lines_to_viewport_rows() {
        let (mut term, _) = parsed(b"first\r\nsecond\r\nthird", 20, 2);
        term.scroll_display(Scroll::Delta(1));
        let view = snapshot(&term, 20, 2);
        assert_eq!(view.display_offset, 1);
        assert_eq!(line_text(&view.lines[0]), "first");
        assert_eq!(line_text(&view.lines[1]), "second");
    }
}
