use std::{collections::BTreeMap, sync::Arc};

use crate::{
    BaseDb,
    base::{FilePosition, FileRange},
    hir::{CallTarget, DefId, DefMap, Expr, LocalResolveResult, MemberTarget, NameRefId},
};

use super::Semantics;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReferenceOccurrenceKind {
    Read,
    Write,
    Call,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReferenceOccurrence {
    target: DefId,
    owner: DefId,
    range: FileRange,
    kind: ReferenceOccurrenceKind,
}

impl ReferenceOccurrence {
    pub const fn target(self) -> DefId {
        self.target
    }

    pub const fn owner(self) -> DefId {
        self.owner
    }

    pub const fn range(self) -> FileRange {
        self.range
    }

    pub const fn kind(self) -> ReferenceOccurrenceKind {
        self.kind
    }
}

#[derive(Clone, Debug, Default)]
pub struct ReferenceIndex {
    by_target: BTreeMap<DefId, Vec<ReferenceOccurrence>>,
    by_owner: BTreeMap<DefId, Vec<ReferenceOccurrence>>,
}

impl ReferenceIndex {
    pub(crate) fn build_cancellable(
        db: Arc<BaseDb>,
        def_map: Arc<DefMap>,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Option<Self> {
        let semantics = Semantics::new(Arc::clone(&db), def_map.clone());
        let mut index = Self::default();

        for definition in def_map
            .definitions()
            .filter(|definition| definition.kind().is_body_owner())
        {
            if is_cancelled() {
                return None;
            }
            let owner = definition.id();
            let Some(body) = db.body(owner) else {
                continue;
            };
            let Some(source_map) = db.body_source_map(owner) else {
                continue;
            };
            let Some(resolution) = db.body_resolution(owner) else {
                continue;
            };
            let inference = db.infer(owner);
            let mut explicit_targets = BTreeMap::<NameRefId, DefId>::new();
            let mut kinds = BTreeMap::<NameRefId, ReferenceOccurrenceKind>::new();

            for (expr_id, expr) in body.exprs() {
                match expr {
                    Expr::Call { callee, .. } => {
                        let Some(Expr::Path(path)) = body.expr(*callee) else {
                            continue;
                        };
                        let Some(name_ref) = path.last().copied() else {
                            continue;
                        };
                        kinds.insert(name_ref, ReferenceOccurrenceKind::Call);
                        if let Some(CallTarget::Definition(target)) = inference
                            .as_ref()
                            .and_then(|result| result.call_info(expr_id))
                            .map(|info| info.target())
                        {
                            explicit_targets.insert(name_ref, target);
                        }
                    }
                    Expr::MethodCall { method, .. } => {
                        kinds.insert(*method, ReferenceOccurrenceKind::Call);
                        if let Some(CallTarget::Definition(target)) = inference
                            .as_ref()
                            .and_then(|result| result.call_info(expr_id))
                            .map(|info| info.target())
                        {
                            explicit_targets.insert(*method, target);
                        }
                    }
                    Expr::Assign { target, .. } => match body.expr(*target) {
                        Some(Expr::Path(path)) => {
                            if let Some(name_ref) = path.last() {
                                kinds.insert(*name_ref, ReferenceOccurrenceKind::Write);
                            }
                        }
                        Some(Expr::Field { field, .. }) => {
                            kinds.insert(*field, ReferenceOccurrenceKind::Write);
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }

            for (name_ref_id, _) in body.name_refs() {
                if is_cancelled() {
                    return None;
                }
                if !matches!(
                    resolution.resolve(name_ref_id),
                    Some(LocalResolveResult::NonLocal)
                ) {
                    continue;
                }
                let Some(range) = source_map.name_ref_range(name_ref_id) else {
                    continue;
                };
                let target = explicit_targets
                    .get(&name_ref_id)
                    .copied()
                    .or_else(|| {
                        inference
                            .as_ref()?
                            .member_resolution(name_ref_id)
                            .and_then(|member| match member.target() {
                                MemberTarget::Definition(target) => Some(target),
                                MemberTarget::Builtin(_) => None,
                            })
                    })
                    .or_else(|| {
                        semantics
                            .find_def_at(FilePosition::new(range.file_id, range.range.start()))
                            .map(|definition| definition.id())
                    });
                let Some(target) = target else {
                    continue;
                };
                index.insert(ReferenceOccurrence {
                    target,
                    owner,
                    range,
                    kind: kinds
                        .get(&name_ref_id)
                        .copied()
                        .unwrap_or(ReferenceOccurrenceKind::Read),
                });
            }
        }

        index.normalize();
        Some(index)
    }

    pub fn occurrences(&self, target: DefId) -> &[ReferenceOccurrence] {
        self.by_target.get(&target).map_or(&[], Vec::as_slice)
    }

    pub fn occurrences_in(&self, owner: DefId) -> &[ReferenceOccurrence] {
        self.by_owner.get(&owner).map_or(&[], Vec::as_slice)
    }

    fn insert(&mut self, occurrence: ReferenceOccurrence) {
        self.by_target
            .entry(occurrence.target)
            .or_default()
            .push(occurrence);
        self.by_owner
            .entry(occurrence.owner)
            .or_default()
            .push(occurrence);
    }

    fn normalize(&mut self) {
        for occurrences in self
            .by_target
            .values_mut()
            .chain(self.by_owner.values_mut())
        {
            occurrences.sort();
            occurrences.dedup();
        }
    }
}
