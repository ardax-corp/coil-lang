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
    /// `__dict1..=n`, read out of `__dict0` after entry from its index
    /// `base` on: `(base, n)`.
    pub(crate) context: Option<(usize, usize)>,
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
        if let Some((_, n)) = dicts.context
            && frame.dicts.contains_key(&0)
        {
            for i in 0..n {
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
                // caller reads ([`Self::param_pair_kind`]).
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
        self.hir_body_frame.as_ref().and_then(|f| f.dict_slot(n))
    }

    /// Whether `name` is a parameter or capture of the body being planned
    /// or lowered.
    pub(crate) fn frame_binds(&self, name: &str) -> bool {
        self.hir_body_frame.as_ref().is_some_and(|f| f.binds(name))
    }
}
