//! UI の LSP 結果が属するプロジェクト・文書・要求を追跡する。

#[derive(Clone, Copy)]
pub enum RequestKind {
    Goto,
    Symbols,
    Hover,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestToken {
    project: u64,
    document: u64,
    request: u64,
}

#[derive(Default)]
pub struct LspEpoch {
    project: u64,
    document: u64,
    requests: [u64; 3],
}

impl LspEpoch {
    pub fn next_project(&mut self) -> u64 {
        self.project += 1;
        self.next_document();
        self.project
    }

    pub fn is_project(&self, project: u64) -> bool {
        self.project == project
    }

    /// 同じパスの再オープンや編集でも旧結果を失効させる。
    pub fn next_document(&mut self) {
        self.document += 1;
    }

    pub fn request(&mut self, kind: RequestKind) -> RequestToken {
        self.requests[kind as usize] += 1;
        RequestToken {
            project: self.project,
            document: self.document,
            request: self.requests[kind as usize],
        }
    }

    pub fn accepts(&self, kind: RequestKind, token: RequestToken) -> bool {
        token.project == self.project
            && token.document == self.document
            && token.request == self.requests[kind as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_latest_request_in_each_view_can_apply() {
        let mut epoch = LspEpoch::default();
        let old = epoch.request(RequestKind::Goto);
        let symbols = epoch.request(RequestKind::Symbols);
        let new = epoch.request(RequestKind::Goto);
        assert!(!epoch.accepts(RequestKind::Goto, old));
        assert!(epoch.accepts(RequestKind::Goto, new));
        assert!(epoch.accepts(RequestKind::Symbols, symbols));
    }

    #[test]
    fn document_reopen_edit_and_project_switch_invalidate_all_views() {
        let mut epoch = LspEpoch::default();
        for project_switch in [false, true] {
            let tokens = [RequestKind::Goto, RequestKind::Symbols, RequestKind::Hover]
                .map(|kind| (kind, epoch.request(kind)));
            if project_switch {
                epoch.next_project();
            } else {
                epoch.next_document();
            }
            for (kind, token) in tokens {
                assert!(!epoch.accepts(kind, token));
            }
        }
    }
}
