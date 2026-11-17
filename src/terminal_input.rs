//! IME変換の確定先をタブの添字から独立して保持する。

#[derive(Default)]
pub struct Composition {
    pub text: String,
    pub selection: (usize, usize),
    pub owner: Option<u64>,
}

impl Composition {
    pub fn mark(&mut self, text: String, selection: (usize, usize), session: Option<u64>) {
        if text.is_empty() {
            self.owner = None;
        } else if self.owner.is_none() {
            self.owner = session;
        }
        self.text = text;
        self.selection = selection;
    }

    pub fn take_target(&mut self, active: Option<u64>) -> Option<u64> {
        let target = self.owner.take().or(active);
        self.text.clear();
        self.selection = (0, 0);
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_keeps_original_owner_across_switch_add_close_and_project_change() {
        for active in [Some(1), Some(2), Some(3), None] {
            let mut ime = Composition::default();
            ime.mark("にほん".into(), (0, 9), Some(1));
            // 候補変更は元の所有者を保持する。
            ime.mark("日本".into(), (0, 6), active);
            assert_eq!(ime.take_target(active), Some(1));
            assert!(ime.text.is_empty());
            assert!(ime.owner.is_none());
        }
    }

    #[test]
    fn cancellation_releases_owner_and_plain_text_uses_active_session() {
        let mut ime = Composition::default();
        ime.mark("あ".into(), (0, 3), Some(1));
        ime.mark(String::new(), (0, 0), Some(2));
        assert_eq!(ime.take_target(Some(2)), Some(2));
        assert_eq!(ime.take_target(None), None);
        ime.mark("a".into(), (1, 1), Some(3));
        assert_eq!(ime.take_target(Some(2)), Some(3));
    }
}
