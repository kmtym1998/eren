//! 可視テキストからエージェントの状態を推定するヒューリスティクス。
//! CLI の表示変更や画面に残った出力によって誤判定する場合がある。

/// 起動するエージェントの種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Shell,
    ClaudeCode,
    Codex,
    OpenCode,
}

impl AgentKind {
    /// 起動コマンド。シェルは通常のログインシェルを使う。
    pub fn command(&self) -> Option<&'static str> {
        match self {
            Self::Shell => None,
            Self::ClaudeCode => Some("claude"),
            Self::Codex => Some("codex"),
            Self::OpenCode => Some("opencode"),
        }
    }

    /// 画面に表示する名前。
    pub fn title(&self) -> &'static str {
        match self {
            Self::Shell => "シェル",
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
        }
    }

    /// 追加ボタンに表示する全種別。
    pub fn all() -> &'static [AgentKind] {
        &[Self::Shell, Self::ClaudeCode, Self::Codex, Self::OpenCode]
    }
}

/// 可視テキストから推定した状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// 起動直後で特徴的な出力がない。
    Starting,
    /// 生成やツール実行などの作業中。
    Running,
    /// ユーザーの入力待ち。
    WaitingForInput,
    /// 判別できない。
    Unknown,
}

/// エージェントの種別と直近の推定状態を保持する。
pub struct StatusDetector {
    kind: AgentKind,
    status: AgentStatus,
}

impl StatusDetector {
    /// 起動直後の検出器を作る。
    pub fn new(kind: AgentKind) -> Self {
        Self {
            kind,
            status: AgentStatus::Starting,
        }
    }

    /// 最新の可視テキスト全体から状態を更新する。
    pub fn observe(&mut self, visible_text: &str) {
        let text = visible_text.trim();
        let lowercase = text.to_ascii_lowercase();
        // 入力欄が作業中にも残る場合があるため、割り込み表示を優先する。
        let running = lowercase.contains("esc to interrupt")
            || (self.kind == AgentKind::Codex
                && lowercase
                    .split(|c: char| !c.is_ascii_alphabetic())
                    .any(|word| word == "working"));
        let waiting = match self.kind {
            AgentKind::Shell => text.lines().next_back().is_some_and(|line| {
                let line = line.trim_end();
                line.ends_with('$') || line.ends_with('%')
            }),
            AgentKind::ClaudeCode => tail_lines(text).any(|line| {
                prompt_line(line, '❯')
                    || line.to_ascii_lowercase().contains("bypass permissions")
                    || line.to_ascii_lowercase().contains("accept edits")
            }),
            AgentKind::Codex => tail_lines(text).any(|line| prompt_line(line, '›')),
            AgentKind::OpenCode => tail_lines(&lowercase).any(|line| line.contains("ask anything")),
        };

        self.status = if running {
            AgentStatus::Running
        } else if waiting {
            AgentStatus::WaitingForInput
        } else if self.status == AgentStatus::Starting && text.chars().count() < 4 {
            AgentStatus::Starting
        } else {
            AgentStatus::Unknown
        };
    }

    /// 直近の推定状態を返す。
    pub fn status(&self) -> AgentStatus {
        self.status
    }

    /// 検出対象の種別を返す（モジュールの公開 API）。
    #[allow(dead_code)]
    pub fn kind(&self) -> AgentKind {
        self.kind
    }
}

// 末尾の空行を除く最大４行を入力欄とフッターの候補にする。
fn tail_lines(text: &str) -> impl Iterator<Item = &str> {
    text.trim_end().lines().rev().take(4).map(str::trim)
}

fn prompt_line(line: &str, marker: char) -> bool {
    line.strip_prefix(marker)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(kind: AgentKind) -> &'static str {
        match kind {
            AgentKind::Shell => "user@host:~ $",
            AgentKind::ClaudeCode => "❯ \n  bypass permissions on\n\n",
            AgentKind::Codex => "› \n  ? for shortcuts\n\n",
            AgentKind::OpenCode => "Ask anything\n\n",
        }
    }

    #[test]
    fn presets() {
        let expected = [
            (AgentKind::Shell, None, "シェル"),
            (AgentKind::ClaudeCode, Some("claude"), "Claude Code"),
            (AgentKind::Codex, Some("codex"), "Codex"),
            (AgentKind::OpenCode, Some("opencode"), "OpenCode"),
        ];
        assert_eq!(AgentKind::all().len(), expected.len());
        for (kind, command, title) in expected {
            assert!(AgentKind::all().contains(&kind));
            assert_eq!(kind.command(), command);
            assert_eq!(kind.title(), title);
        }
    }

    #[test]
    fn lifecycle_for_every_kind() {
        for &kind in AgentKind::all() {
            let mut detector = StatusDetector::new(kind);
            assert_eq!(detector.kind(), kind);
            assert_eq!(detector.status(), AgentStatus::Starting);
            for text in ["", " \n ", "a", "…"] {
                detector.observe(text);
                assert_eq!(detector.status(), AgentStatus::Starting, "{kind:?}");
            }
            detector.observe(prompt(kind));
            assert_eq!(detector.status(), AgentStatus::WaitingForInput, "{kind:?}");
            detector.observe(&format!("{}\nesc to interrupt", prompt(kind)));
            assert_eq!(detector.status(), AgentStatus::Running, "{kind:?}");
            detector.observe(prompt(kind));
            assert_eq!(detector.status(), AgentStatus::WaitingForInput, "{kind:?}");
            detector.observe("unrecognized output");
            assert_eq!(detector.status(), AgentStatus::Unknown, "{kind:?}");
        }
    }

    #[test]
    fn codex_working_takes_priority_over_prompt() {
        let mut detector = StatusDetector::new(AgentKind::Codex);
        detector.observe("• Working (2s)\n› ");
        assert_eq!(detector.status(), AgentStatus::Running);
        detector.observe("› ");
        assert_eq!(detector.status(), AgentStatus::WaitingForInput);
        detector.observe("networking");
        assert_eq!(detector.status(), AgentStatus::Unknown);
    }

    #[test]
    fn claude_footer_context() {
        for footer in ["bypass permissions on", "accept edits on"] {
            let mut detector = StatusDetector::new(AgentKind::ClaudeCode);
            detector.observe(footer);
            assert_eq!(detector.status(), AgentStatus::WaitingForInput);
        }
    }

    #[test]
    fn old_prompts_and_inline_markers_are_not_input_fields() {
        for (kind, marker) in [(AgentKind::ClaudeCode, "❯"), (AgentKind::Codex, "›")] {
            let mut detector = StatusDetector::new(kind);
            detector.observe(&format!(
                "{marker}\nline one\nline two\nline three\nline four"
            ));
            assert_eq!(detector.status(), AgentStatus::Unknown);
            detector.observe(&format!("output includes {marker} here"));
            assert_eq!(detector.status(), AgentStatus::Unknown);
        }
    }

    #[test]
    fn shell_uses_last_nonblank_line() {
        let mut detector = StatusDetector::new(AgentKind::Shell);
        for text in ["$", "%", "user@host %  \n\n"] {
            detector.observe(text);
            assert_eq!(detector.status(), AgentStatus::WaitingForInput);
        }
        detector.observe("user@host $\ncommand output");
        assert_eq!(detector.status(), AgentStatus::Unknown);
    }
}
