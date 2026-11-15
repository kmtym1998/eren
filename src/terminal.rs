//! alacritty_terminal を使った PTY ターミナル。
//!
//! シェルを PTY で起動し、VT パースとグリッド管理は alacritty_terminal に任せる。
//! 描画は Workbench 側で、ここで作るスナップショット（行ごとのスタイル付き run）を表示する。

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
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
}

impl EventListener for WakeListener {
    fn send_event(&self, event: Event) {
        if matches!(event, Event::Wakeup) {
            self.wake.store(true, Ordering::SeqCst);
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
}

pub struct TerminalSession {
    term: Arc<FairMutex<Term<WakeListener>>>,
    sender: EventLoopSender,
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
        let listener = WakeListener { wake: wake.clone() };
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

    /// 現在の表示グリッドを行ごとの run 列に変換する。
    pub fn snapshot(&self) -> Snapshot {
        let term = self.term.lock();
        let content = term.renderable_content();
        let colors = content.colors;
        let default_fg = colors[NamedColor::Foreground].unwrap_or(DEFAULT_FG);
        let default_bg = colors[NamedColor::Background].unwrap_or(DEFAULT_BG);
        let show_cursor = content.mode.contains(TermMode::SHOW_CURSOR)
            && content.cursor.shape == CursorShape::Block
            && content.display_offset == 0;
        let cursor_point = content.cursor.point;

        let resolve = |color: Color, is_fg: bool| -> Rgb {
            let fallback = if is_fg { default_fg } else { default_bg };
            match color {
                Color::Spec(rgb) => rgb,
                Color::Indexed(idx) => colors[idx as usize].unwrap_or(fallback),
                Color::Named(named) => colors[named].unwrap_or(fallback),
            }
        };

        // セルを (文字, fg, bg, bold) のグリッドに展開する
        let mut grid: Vec<Vec<(char, u32, u32, bool)>> =
            vec![
                vec![(' ', rgb_u32(default_fg), rgb_u32(default_bg), false); self.cols];
                self.rows
            ];
        for indexed in content.display_iter {
            let line = indexed.point.line.0;
            let col = indexed.point.column.0;
            if line < 0 || line as usize >= self.rows || col >= self.cols {
                continue;
            }
            let cell = indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let mut fg = resolve(cell.fg, true);
            let mut bg = resolve(cell.bg, false);
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
        }
    }
}
