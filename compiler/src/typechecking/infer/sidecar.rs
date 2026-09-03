//! Typed facts after `check_program`, keyed only by [`NodeId`] / [`DefId`].
//!
//! Codegen still walks `Expression` for shape. Meaning (overload, dicts,
//! ForInKind, FFI tags) prefers this table over span/`String` maps. Span
//! fallbacks remain on [`Checker`] until their tests move.

use std::collections::{HashMap, HashSet};

use crate::typechecking::def_id::DefId;
use crate::typechecking::generics::InstanceDef;
use crate::typechecking::id::NodeId;
use crate::typechecking::purity::EffectFlags;
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
    /// ObjEnum / small class values proven never to leave this frame.
    frame_local: HashSet<NodeId>,
    /// Last in-frame use of a frame-local (drop payload/tag after this node).
    frame_local_last_use: HashSet<NodeId>,
    /// Index expressions proven `0 <= i < len(arr)` with stable length.
    in_bounds_index: HashSet<NodeId>,
    /// Array parameter nodes to pin for the frame (`ArrayPin`).
    pin_array: HashSet<NodeId>,
    pin_params: HashSet<(String, String)>,
    for_in_pin: HashSet<NodeId>,
    for_in_pin_spans: HashSet<(usize, usize)>,
    /// Effect bits per function DefId (empty = pure). Missing DefId is unknown/impure.
    fn_effects: HashMap<DefId, EffectFlags>,
    pure_fn_names: HashSet<String>,
    /// Ground functions whose return type uses the two-slot CALL/RETURN ABI.
    two_word_returns: HashSet<DefId>,
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

    /// True when `id` is a non-escaping in-frame ObjEnum / small class.
    pub fn is_frame_local(&self, id: NodeId) -> bool {
        self.frame_local.contains(&id)
    }

    pub fn frame_local_ids(&self) -> &HashSet<NodeId> {
        &self.frame_local
    }

    /// True when `id` is the last in-frame use of a frame-local value.
    pub fn is_frame_local_last_use(&self, id: NodeId) -> bool {
        self.frame_local_last_use.contains(&id)
    }

    /// True when `id` is an `arr[i]` proven in-bounds with a stable length.
    pub fn is_in_bounds_index(&self, id: NodeId) -> bool {
        self.in_bounds_index.contains(&id)
    }

    pub fn in_bounds_index_ids(&self) -> &HashSet<NodeId> {
        &self.in_bounds_index
    }

    /// True when `id` is an array parameter that may be pinned at function entry.
    pub fn is_pin_array(&self, id: NodeId) -> bool {
        self.pin_array.contains(&id)
    }

    pub fn is_pin_param(&self, fn_name: &str, param: &str) -> bool {
        let short = fn_name.rsplit("::").next().unwrap_or(fn_name);
        self.pin_params.contains(&(fn_name.to_string(), param.to_string()))
            || self.pin_params.contains(&(short.to_string(), param.to_string()))
    }

    /// True when `id` is a for-in loop whose synthetic index is in-bounds.
    pub fn is_for_in_pin(&self, id: NodeId) -> bool {
        self.for_in_pin.contains(&id)
    }

    pub fn is_for_in_pin_span(&self, start: usize, end: usize) -> bool {
        self.for_in_pin_spans.contains(&(start, end))
    }

    /// True when `id` is a user `fn` the checker proved effect-free.
    pub fn is_pure_def(&self, id: DefId) -> bool {
        self.fn_effects.get(&id).is_some_and(|f| f.is_pure())
    }

    pub fn effects(&self, id: DefId) -> Option<EffectFlags> {
        self.fn_effects.get(&id).copied()
    }

    /// Bind names of proven-pure user functions (LICM / PureCallCtx).
    pub fn pure_fn_names(&self) -> &HashSet<String> {
        &self.pure_fn_names
    }

    pub fn name_is_pure(&self, name: &str) -> bool {
        let stem = name.split("$mono$").next().unwrap_or(name);
        if self.pure_fn_names.contains(stem) {
            return true;
        }
        match stem.rsplit_once("::") {
            Some((prefix, short)) if !prefix.contains("::") => self.pure_fn_names.contains(short),
            _ => false,
        }
    }

    /// True when `id` is a function whose direct CALL/RETURN uses two stack slots.
    pub fn is_two_word_return(&self, id: DefId) -> bool {
        self.two_word_returns.contains(&id)
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

        let mut two_word_returns = HashSet::new();
        for (name, def) in &self.local_defs {
            if let Some(ty) = self.fn_return_ty(name)
                && crate::typechecking::return_layout::two_word_return_kind(self, &ty).is_some()
            {
                two_word_returns.insert(*def);
            }
        }

        TypedSidecar {
            tys,
            tys_by_span,
            def_ids: self.def_ids_by_node.clone(),
            overloads,
            dicts: self.call_site_dicts.clone(),
            for_in: self.for_in_infos.clone(),
            ffi_tags,
            frame_local: self.frame_local.clone(),
            frame_local_last_use: self.frame_local_last_use.clone(),
            in_bounds_index: self.in_bounds_index.clone(),
            pin_array: self.pin_array.clone(),
            pin_params: self.pin_params.clone(),
            for_in_pin: self.for_in_pin.clone(),
            for_in_pin_spans: self.for_in_pin_spans.clone(),
            fn_effects: self.fn_effects.clone(),
            pure_fn_names: self.pure_fn_names.clone(),
            two_word_returns,
        }
    }
}
