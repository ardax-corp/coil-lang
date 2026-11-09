//! A body's entry frame, numbered from its HIR.
//!
//! Entry slot `k` is incoming word `k` (the calling convention): each
//! parameter in order (a two-word one in two slots), then the dictionary
//! parameters. A bounded instance method's context dictionaries follow the
//! entry words; they are read out of its own dictionary on entry. The body's
//! locals start after them, at [`BodyFrame::frame_base`]. A lambda's frame
//! starts with its captures, then the enclosing body's dictionaries it
//! captures, then its parameters.

use std::collections::BTreeMap;

use super::*;
use crate::hir::HirBody;

/// The dictionary parameters a body takes, and the context dictionaries it
/// loads on entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrameDicts {
    /// `__dict0..arity`, after the declared parameters.
    pub(crate) arity: usize,
    /// A trait method's own bounds (#1471): `__dict0`, then `n` dictionaries
    /// from `__dict{base}`.
    pub(crate) own: Option<(usize, usize)>,
    /// A bounded generic instance method's context dictionaries,
    /// `__dict1..=n`, read out of `__dict0` after entry.
    pub(crate) context: usize,
}

/// A parameter's or capture's place in the frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FrameLocal {
    pub(crate) name: String,
    pub(crate) slot: u32,
    /// A two-word one's second slot and its kind.
    pub(crate) pair: Option<(u32, PairKind)>,
}

/// Where a body's parameters, `self` and dictionaries live in its frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BodyFrame {
    /// A lambda's captures, in [`HirBody::captures`] order.
    pub(crate) captures: Vec<FrameLocal>,
    /// [`HirBody::params`], in order.
    pub(crate) params: Vec<FrameLocal>,
    /// The slot of `__dictN`, by `N`.
    pub(crate) dicts: BTreeMap<u32, u32>,
    /// Words the caller passes.
    pub(crate) entry_sp: u32,
    /// The first slot past the entry words and the context dictionaries.
    pub(crate) frame_base: u32,
}

impl BodyFrame {
    /// A function's frame: `hir`'s parameters, each two words when `pairs`
    /// gives it a kind, then `dicts`.
    pub(crate) fn function(hir: &HirBody, mut pairs: impl FnMut(usize) -> Option<PairKind>, dicts: FrameDicts) -> Self {
        let mut frame = Self::default();
        for (i, &p) in hir.params.iter().enumerate() {
            let local = frame.local(hir.local(p).name.clone(), pairs(i));
            frame.params.push(local);
        }
        for n in 0..dicts.arity {
            frame.dict(n);
        }
        if let Some((base, n)) = dicts.own {
            frame.dict(0);
            for i in 0..n {
                frame.dict(base + i);
            }
        }
        frame.entry_sp = frame.frame_base;
        // Read out of the instance's own dictionary.
        if frame.dicts.contains_key(&0) {
            for i in 0..dicts.context {
                frame.dict(i + 1);
            }
        }
        frame
    }

    /// A function's frame of one-word parameters and no dictionaries.
    pub(crate) fn plain(hir: &HirBody) -> Self {
        Self::function(hir, |_| None, FrameDicts::default())
    }

    /// A lambda's frame: `lam`'s captures (two words where `pairs` has a
    /// kind), the enclosing body's dictionaries `dicts` it captures, then its
    /// parameters.
    pub(crate) fn lambda(lam: &HirBody, pairs: &[Option<PairKind>], dicts: impl IntoIterator<Item = usize>) -> Self {
        let mut frame = Self::default();
        for (i, &(_, inner)) in lam.captures.iter().enumerate() {
            let local = frame.local(lam.local(inner).name.clone(), pairs.get(i).cloned().flatten());
            frame.captures.push(local);
        }
        for n in dicts {
            frame.dict(n);
        }
        for &p in &lam.params {
            let local = frame.local(lam.local(p).name.clone(), None);
            frame.params.push(local);
        }
        frame.entry_sp = frame.frame_base;
        frame
    }

    /// `__dict{n}`'s slot.
    pub(crate) fn dict_slot(&self, n: usize) -> Option<u32> {
        self.dicts.get(&(n as u32)).copied()
    }

    /// Whether `name` is a parameter or capture of the body.
    pub(crate) fn binds(&self, name: &str) -> bool {
        self.params.iter().chain(&self.captures).any(|l| l.name == name)
    }

    fn local(&mut self, name: String, pair: Option<PairKind>) -> FrameLocal {
        let slot = self.frame_base;
        self.frame_base += 1 + u32::from(pair.is_some());
        FrameLocal { name, slot, pair: pair.map(|kind| (slot + 1, kind)) }
    }

    /// `__dict{n}`'s slot, the next one when it has none yet.
    fn dict(&mut self, n: usize) {
        let next = self.frame_base;
        if let std::collections::btree_map::Entry::Vacant(e) = self.dicts.entry(n as u32) {
            e.insert(next);
            self.frame_base += 1;
        }
    }
}

impl Compiler {
    /// The frame of function body `hir` with dictionaries `dicts`.
    pub(crate) fn hir_function_frame(&self, hir: &HirBody, dicts: FrameDicts) -> BodyFrame {
        let key = self.current_fn_pair_key();
        BodyFrame::function(
            hir,
            |i| {
                let local = hir.local(hir.params[i]);
                // `self` is one word; the rest follow the signature every
                // caller reads ([`Self::argument_unboxed_range_kind`]).
                if self.compiling_method && i == 0 && local.name == "self" {
                    return None;
                }
                self.param_pair_kind(key.as_deref()?, &local.name, local.ty.as_ref())
            },
            dicts,
        )
    }

    /// `__dict{n}`'s slot in the frame of the body being planned or lowered.
    pub(crate) fn frame_dict(&self, n: usize) -> Option<u32> {
        let slot = self.hir_body_frame.as_ref().and_then(|f| f.dict_slot(n));
        debug_assert_eq!(slot, self.lookup_slot(&format!("__dict{n}")), "`__dict{n}`'s slot");
        slot
    }

    /// Whether `name` is a parameter or capture of the body being planned
    /// or lowered.
    pub(crate) fn frame_binds(&self, name: &str) -> bool {
        let binds = self.hir_body_frame.as_ref().is_some_and(|f| f.binds(name));
        debug_assert_eq!(binds, self.lookup_slot(name).is_some(), "whether the frame binds `{name}`");
        binds
    }

    /// Debug builds: `frame` numbers `hir`'s entry frame as the interned
    /// names in `context.variables` do.
    pub(crate) fn debug_check_body_frame(&self, hir: &HirBody, frame: &BodyFrame) {
        if !cfg!(debug_assertions) {
            return;
        }
        let interned = self.interned_body_frame(hir, frame);
        debug_assert!(
            interned == *frame,
            "frame of `{}` numbered from HIR differs from the interned one:\n  hir:      {frame:?}\n  interned: {interned:?}",
            hir.name
        );
    }

    /// The frame `context.variables` holds for `hir` (`frame` gives the
    /// captures' pair kinds and the entry words, which names do not record).
    fn interned_body_frame(&self, hir: &HirBody, frame: &BodyFrame) -> BodyFrame {
        let vars = &self.context.variables;
        let slot = |name: &str| vars.key(&name.to_string()).map_or(u32::MAX, |s| s as u32);
        let kinds = frame.captures.iter().map(|c| c.pair.as_ref().map(|(_, kind)| kind)).chain(std::iter::repeat(None));
        let captures = hir
            .captures
            .iter()
            .zip(kinds)
            .map(|(&(_, inner), kind)| {
                let name = hir.local(inner).name.clone();
                let pair = kind.map(|kind| (slot(&format!("{name}$hi")), kind.clone()));
                FrameLocal { slot: slot(&name), name, pair }
            })
            .collect();
        let params = hir
            .params
            .iter()
            .map(|&p| {
                let name = hir.local(p).name.clone();
                match self.unboxed_enum_kind(&name).cloned().zip(self.unboxed_enum_info(&name)) {
                    Some((kind, (start, tag))) => FrameLocal { name, slot: start, pair: Some((tag, kind)) },
                    None => FrameLocal { slot: slot(&name), name, pair: None },
                }
            })
            .collect();
        let dicts = vars
            .iter()
            .enumerate()
            .filter_map(|(slot, name)| Some((name.strip_prefix("__dict")?.parse().ok()?, slot as u32)))
            .collect();
        BodyFrame { captures, params, dicts, entry_sp: frame.entry_sp, frame_base: vars.len() as u32 }
    }
}
