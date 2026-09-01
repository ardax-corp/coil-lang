//! Typed facts after `check_program`, keyed only by [`NodeId`] / [`DefId`].
//!
//! Codegen still walks `Expression` for shape. Meaning (overload, dicts,
//! ForInKind, FFI tags) prefers this table over span/`String` maps. Span
//! fallbacks remain on [`Checker`] until their tests move.

use std::collections::HashMap;

use crate::typechecking::def_id::DefId;
use crate::typechecking::generics::InstanceDef;
use crate::typechecking::id::NodeId;
use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::ty::Ty;

use super::{Checker, ForInInfo};

/// Call-site overload selection recorded on a [`NodeId`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectedOverload {
    pub fixed_arity: usize,
    pub is_rest: bool,
    pub candidate_id: u32,
}

/// Checker snapshot keyed only by [`NodeId`] / [`DefId`]. No span maps.
#[derive(Clone, Debug, Default)]
pub struct TypedSidecar {
    tys: HashMap<NodeId, Ty>,
    tys_by_span: HashMap<(usize, usize), Ty>,
    def_ids: HashMap<NodeId, DefId>,
    overloads: HashMap<NodeId, SelectedOverload>,
    dicts: HashMap<NodeId, Vec<InstanceDef>>,
    for_in: HashMap<NodeId, ForInInfo>,
    ffi_tags: HashMap<DefId, Vec<u32>>,
}

impl TypedSidecar {
    pub fn ty(&self, id: NodeId) -> Option<&Ty> {
        self.tys.get(&id)
    }

    pub fn ty_at_span(&self, start: usize, end: usize) -> Option<&Ty> {
        self.tys_by_span.get(&(start, end))
    }

    pub fn def_id(&self, id: NodeId) -> Option<DefId> {
        self.def_ids.get(&id).copied()
    }

    pub fn overload(&self, id: NodeId) -> Option<SelectedOverload> {
        self.overloads.get(&id).copied()
    }

    pub fn dicts(&self, id: NodeId) -> Option<&[InstanceDef]> {
        self.dicts.get(&id).map(Vec::as_slice)
    }

    pub fn for_in(&self, id: NodeId) -> Option<&ForInInfo> {
        self.for_in.get(&id)
    }

    pub fn ffi_tags(&self, id: DefId) -> Option<&[u32]> {
        self.ffi_tags.get(&id).map(Vec::as_slice)
    }

    pub fn tys(&self) -> &HashMap<NodeId, Ty> {
        &self.tys
    }
}

impl Checker {
    /// Snapshot NodeId / DefId facts after [`Checker::check_program`].
    pub fn typed_sidecar(&self) -> TypedSidecar {
        let subst = &self.subst;
        let mut tys = HashMap::with_capacity(self.cache.len());
        for (id, ty) in &self.cache {
            tys.insert(*id, apply_ty_prune(subst, ty));
        }

        let mut overloads = HashMap::with_capacity(self.selected_overloads.len());
        for (id, &(fixed_arity, is_rest, candidate_id)) in &self.selected_overloads {
            overloads.insert(
                *id,
                SelectedOverload {
                    fixed_arity,
                    is_rest,
                    candidate_id,
                },
            );
        }

        let mut ffi_tags = HashMap::new();
        for (name, def) in &self.local_defs {
            if let Some(tags) = self.ffi_fn_arg_tags.get(name) {
                ffi_tags.insert(*def, tags.clone());
            }
        }

        let mut tys_by_span = HashMap::with_capacity(self.codegen_types_by_span.len());
        for (&span, ty) in &self.codegen_types_by_span {
            tys_by_span.insert(span, apply_ty_prune(subst, ty));
        }

        TypedSidecar {
            tys,
            tys_by_span,
            def_ids: self.def_ids_by_node.clone(),
            overloads,
            dicts: self.call_site_dicts.clone(),
            for_in: self.for_in_infos.clone(),
            ffi_tags,
        }
    }
}
