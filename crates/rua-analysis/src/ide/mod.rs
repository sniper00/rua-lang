//! Snapshot-based IDE API, including `AnalysisHost` and feature queries.
//!
//! Results exposed here remain independent of LSP protocol types.

use std::{rc::Rc, sync::Arc};

use rua_syntax::{Parse, ast::SourceFile};

use crate::{BaseDb, diagnostic::Diagnostic, vfs::Change, vfs::FileId};

/// Mutable owner of the current analysis inputs.
#[derive(Debug, Default)]
pub struct AnalysisHost {
    db: Rc<BaseDb>,
}

impl AnalysisHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_change(&mut self, change: Change) {
        Rc::make_mut(&mut self.db).apply_change(change);
    }

    pub fn analysis(&self) -> Analysis {
        Analysis {
            db: Rc::clone(&self.db),
        }
    }
}

/// Immutable view of the inputs captured when the snapshot was created.
#[derive(Clone, Debug)]
pub struct Analysis {
    db: Rc<BaseDb>,
}

impl Analysis {
    /// Test and integration helper for syntax queries during Phase 2.
    pub fn parse(&self, file_id: FileId) -> Arc<Parse<SourceFile>> {
        self.db.parse(file_id)
    }

    /// Semantic diagnostics are introduced after the analysis skeleton.
    pub fn diagnostics(&self, _file_id: FileId) -> Vec<Diagnostic> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::AnalysisHost;
    use crate::vfs::{Change, FileId};

    #[test]
    fn analysis_host_applies_changes_and_exposes_parse() {
        let file_id = FileId::new(0);
        let mut change = Change::new();
        change.set_file_text(file_id, "fn main() {}");

        let mut host = AnalysisHost::new();
        host.apply_change(change);
        let analysis = host.analysis();

        assert_eq!(
            analysis.parse(file_id).syntax_node().text().to_string(),
            "fn main() {}"
        );
        assert!(analysis.diagnostics(file_id).is_empty());
    }

    #[test]
    fn analysis_host_snapshots_are_isolated_from_later_changes() {
        let file_id = FileId::new(0);
        let mut initial = Change::new();
        initial.set_file_text(file_id, "fn before() {}");

        let mut host = AnalysisHost::new();
        host.apply_change(initial);
        let before = host.analysis();

        let mut update = Change::new();
        update.set_file_text(file_id, "fn after() {}");
        host.apply_change(update);
        let after = host.analysis();

        assert_eq!(
            before.parse(file_id).syntax_node().text().to_string(),
            "fn before() {}"
        );
        assert_eq!(
            after.parse(file_id).syntax_node().text().to_string(),
            "fn after() {}"
        );
    }

    #[test]
    fn analysis_host_applies_a_change_batch_in_order() {
        let file_id = FileId::new(0);
        let mut change = Change::new();
        change.set_file_text(file_id, "fn first() {}");
        change.remove_file(file_id);
        change.set_file_text(file_id, "fn last() {}");

        let mut host = AnalysisHost::new();
        host.apply_change(change);

        assert_eq!(
            host.analysis()
                .parse(file_id)
                .syntax_node()
                .text()
                .to_string(),
            "fn last() {}"
        );
    }
}
