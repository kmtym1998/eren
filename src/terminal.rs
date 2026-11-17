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
    /// 表示中のカーソル位置 (列, 行)。履歴表示中やカーソル非表示なら None。
    pub cursor: Option<(usize, usize)>,
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
        Self::spawn(cwd, cols, rows, None)
    }

    /// `cwd` でエージェント等のコマンドを起動する。コマンドが終わったらシェルへ落ちる。
    pub fn new_agent(cwd: &Path, cols: usize, rows: usize, command: &str) -> std::io::Result<Self> {
        Self::spawn(cwd, cols, rows, Some(command))
    }

    fn spawn(cwd: &Path, cols: usize, rows: usize, command: Option<&str>) -> std::io::Result<Self> {
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
        let args = match command {
            Some(command) => vec![
                "-l".to_string(),
                "-c".to_string(),
                format!("{command}; exec {shell_path} -l"),
            ],
            None => vec!["-l".to_string()],
        };
        let options = TtyOptions {
            shell: Some(Shell::new(shell_path, args)),
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

    /// 画面の可視テキスト（エージェントの状態判定用）。
    pub fn visible_text(&self) -> String {
        let snapshot = self.snapshot();
        let mut text = String::new();
        for line in &snapshot.lines {
            for run in &line.runs {
                text.push_str(&run.text);
            }
            text.push('\n');
        }
        text
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

    /// 現在の表示グリッドを行ごとの run 列に変換する。
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
    let cursor_point = content.cursor.point;
    let cursor = if show_cursor && cursor_point.line.0 >= 0 {
        Some((cursor_point.column.0, cursor_point.line.0 as usize))
    } else {
        None
    };

    let resolve = |color: Color| -> Rgb {
        match color {
            Color::Spec(rgb) => rgb,
            Color::Indexed(idx) => palette_color(colors, idx as usize),
            Color::Named(named) => palette_color(colors, named as usize),
        }
    };

    // セルを (文字, fg, bg, bold) のグリッドに展開する
    let mut grid: Vec<Vec<(char, u32, u32, bool)>> =
        vec![vec![(' ', rgb_u32(default_fg), rgb_u32(default_bg), false); cols]; rows];
    for indexed in content.display_iter {
        let line = indexed.point.line.0;
        let col = indexed.point.column.0;
        if line < 0 || line as usize >= rows || col >= cols {
            continue;
        }
        let cell = indexed.cell;
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
            || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let mut fg = resolve(cell.fg);
        let mut bg = resolve(cell.bg);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if show_cursor && indexed.point == cursor_point {
            std::mem::swap(&mut fg, &mut bg);
        }
        let ch = if cell.flags.contains(Flags::HIDDEN) || cell.c == '\0' {
            ' '
        } else {
            cell.c
        };
        grid[line as usize][col] = (
            ch,
            rgb_u32(fg),
            rgb_u32(bg),
            cell.flags.contains(Flags::BOLD),
        );
    }

    // 行ごとに同じスタイルのセルを run にまとめる（末尾の空白は既定背景なら落とす）
    let default_bg_u32 = rgb_u32(default_bg);
    let lines: Vec<Line> = grid
        .into_iter()
        .map(|cells| {
            let mut end = cells.len();
            while end > 0 {
                let (ch, _, bg, _) = cells[end - 1];
                if ch == ' ' && bg == default_bg_u32 {
                    end -= 1;
                } else {
                    break;
                }
            }
            let mut runs: Vec<Run> = Vec::new();
            for (ch, fg, bg, bold) in &cells[..end] {
                match runs.last_mut() {
                    Some(run) if run.fg == *fg && run.bg == *bg && run.bold == *bold => {
                        run.text.push(*ch);
                    }
                    _ => runs.push(Run {
                        text: ch.to_string(),
                        fg: *fg,
                        bg: *bg,
                        bold: *bold,
                    }),
                }
            }
            Line { runs }
        })
        .collect();
    Snapshot {
        lines,
        default_bg: default_bg_u32,
        display_offset: content.display_offset,
        cursor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type Processor = alacritty_terminal::vte::ansi::Processor;

    fn parsed(bytes: &[u8]) -> (Term<WakeListener>, mpsc::Receiver<Event>) {
        let (events, rx) = mpsc::channel();
        let listener = WakeListener {
            wake: Arc::new(AtomicBool::new(false)),
            events,
        };
        let mut term = Term::new(
            Config::default(),
            &TermSize {
                cols: 80,
                lines: 24,
            },
            listener,
        );
        Processor::new().advance(&mut term, bytes);
        (term, rx)
    }

    #[test]
    fn protocol_replies_preserve_scrollback() {
        let (mut term, events) = parsed(b"abc\x1b[6n\x1b[5n\x1b]11;?\x07\x1b[18t\x1b[14t");
        Processor::new().advance(&mut term, "\r\n".repeat(30).as_bytes());
        term.scroll_display(Scroll::Delta(3));
        let offset = term.grid().display_offset();
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
                "\x1b[8;24;80t",
                "\x1b[4;408;560t"
            ]
        );
        assert_eq!(term.grid().display_offset(), offset);
    }

    #[test]
    fn palette_matches_ansi_cube_grayscale_and_osc_changes() {
        let (mut term, events) = parsed(b"\x1b]4;1;rgb:12/34/56\x07\x1b]4;1;?\x07");
        assert_eq!(rgb_u32(palette_color(term.colors(), 1)), 0x123456);
        let replies: Vec<_> = events
            .try_iter()
            .filter_map(|event| reply_to_event(event, &term, window_size(80, 24)))
            .collect();
        assert_eq!(replies, ["\x1b]4;1;rgb:1212/3434/5656\x07"]);
        Processor::new().advance(&mut term, b"\x1b]104;1\x07");
        assert_eq!(rgb_u32(palette_color(term.colors(), 1)), 0xcd0000);
        for (index, color) in [
            (0, 0),
            (9, 0xff0000),
            (16, 0),
            (46, 0x00ff00),
            (231, 0xffffff),
            (232, 0x080808),
            (255, 0xeeeeee),
        ] {
            assert_eq!(rgb_u32(palette_color(term.colors(), index)), color);
        }
    }

    #[test]
    fn renders_foreground_background_truecolor_and_inverse() {
        let (term, _) =
            parsed(b"\x1b[?25l\x1b[31mR\x1b[38;5;46mG\x1b[38;2;1;2;3mT\x1b[0;44mB\x1b[7mI");
        let view = snapshot(&term, 80, 24);
        let runs = &view.lines[0].runs;
        assert_eq!(
            runs.iter().map(|r| r.fg).collect::<Vec<_>>(),
            [0xcd0000, 0x00ff00, 0x010203, rgb_u32(DEFAULT_FG), 0x0000ee]
        );
        assert_eq!(runs[3].bg, 0x0000ee);
        assert_eq!(runs[4].bg, rgb_u32(DEFAULT_FG));
    }

    #[test]
    fn osc_default_colors_reset_for_rendering_and_queries() {
        let (mut term, events) =
            parsed(b"\x1b[?25l\x1b]10;#112233\x07\x1b]11;#445566\x07X\x1b]10;?\x07\x1b]11;?\x07");
        let view = snapshot(&term, 80, 24);
        assert_eq!(view.lines[0].runs[0].fg, 0x112233);
        assert_eq!(view.default_bg, 0x445566);
        let replies: Vec<_> = events
            .try_iter()
            .filter_map(|event| reply_to_event(event, &term, window_size(80, 24)))
            .collect();
        assert_eq!(
            replies,
            [
                "\x1b]10;rgb:1111/2222/3333\x07",
                "\x1b]11;rgb:4444/5555/6666\x07"
            ]
        );
        Processor::new().advance(&mut term, b"\x1b]110\x07\x1b]111\x07");
        let view = snapshot(&term, 80, 24);
        assert_eq!(view.lines[0].runs[0].fg, rgb_u32(DEFAULT_FG));
        assert_eq!(view.default_bg, rgb_u32(DEFAULT_BG));
    }

    #[test]
    fn real_pty_replies_are_routed_to_each_session() {
        let fixture = r#"
import os, select, sys, termios, tty
old = termios.tcgetattr(0)
tty.setraw(0)
try:
    rows, cols = os.get_terminal_size(0).lines, os.get_terminal_size(0).columns
    for request, expected in [(b'\x1b[6n', b'\x1b[1;1R'), (b'\x1b[18t', ('\x1b[8;%d;%dt' % (rows, cols)).encode()), (b'\x1b]4;1;?\x07', b'\x1b]4;1;rgb:cdcd/0000/0000\x07')]:
        os.write(1, request)
        result = b''
        while len(result) < len(expected):
            assert select.select([0], [], [], 3)[0], ('timeout', request, result)
            result += os.read(0, 1024)
        assert result == expected, (result, expected)
finally:
    termios.tcsetattr(0, termios.TCSANOW, old)
print('protocol-fixture-ok', flush=True)
"#;
        let command = format!(
            "exec /usr/bin/python3 -c '{}'",
            fixture.replace('\'', "'\\''")
        );
        let a = TerminalSession::new_agent(Path::new("/private/tmp"), 80, 24, &command).unwrap();
        let b = TerminalSession::new_agent(Path::new("/private/tmp"), 40, 10, &command).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut success = false;
        while std::time::Instant::now() < deadline {
            a.process_events();
            b.process_events();
            if a.visible_text().contains("protocol-fixture-ok")
                && b.visible_text().contains("protocol-fixture-ok")
            {
                success = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let output = (a.visible_text(), b.visible_text());
        a.shutdown();
        b.shutdown();
        assert!(success, "PTY fixture failed: {output:?}");
    }
}
