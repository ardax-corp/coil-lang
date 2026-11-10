//! A body's entry frame, numbered from its HIR.
//!
//! Entry slot `k` is incoming word `k` (the calling convention): each
//! parameter in order (a two-word one in two slots), then the dictionary
//! parameters. A bounded instance method's context dictionaries follow the
//! entry words; they are read out of its own dictionary on entry. The body's
//! locals start after them, at [`BodyFrame::frame_base`]. A lambda's frame
//! starts with its captures, then the enclosing body's dictionaries it
//! captures, then its parameters.
//!
//! A shared generic entry's last word is its `kinds` mask (#2506): bit
//! `i` set when its type parameter `i` is a copy-on-write object at the
//! call ([`FrameKinds`]). A lambda of such a body captures it after the
//! dictionaries.

use std::collections::BTreeMap;

use super::*;
use crate::hir::HirBody;
use crate::typechecking::ty::TyVarId;

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
    /// A shared generic entry's `kinds` word, after the dictionaries.
    pub(crate) kinds: bool,
    /// A trait method's `kinds` word in its own dictionary (`__dict0`):
    /// its index there and how many bits it has (#2506).
    pub(crate) kinds_dict: Option<(u32, u32)>,
}

/// A shared generic body's `kinds` word (#2506): its slot, and the bit of
/// each type variable its code names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrameKinds {
    /// The entry word, for a shared generic entry or a lambda of one.
    pub(crate) slot: Option<u32>,
    /// The word read out of the body's own dictionary at entry: that
    /// dictionary's slot, the index, and how many bits it has. Its bits come
    /// first; the entry word's, where there is one too, after them.
    pub(crate) dict: Option<(u32, u32, u32)>,
    /// Each type variable's bit, in the scheme's order.
    pub(crate) bits: Vec<(TyVarId, u32)>,
    /// How many type parameters the mask has.
    pub(crate) n: u32,
    /// `self`'s type in the scheme (`Cell<T>`), where the body's own `self`
    /// is typed by its class alone: what a sibling call's receiver binds.
    pub(crate) self_ty: Option<Ty>,
    /// The panic message index of the entry's sentinel check, in debug
    /// builds and under `COIL_MIR_VERIFY`.
    pub(crate) check: Option<u32>,
}

impl FrameKinds {
    /// Type variable `v`'s bit.
    pub(crate) fn bit(&self, v: TyVarId) -> Option<u32> {
        self.bits.iter().find(|&&(w, _)| w == v).map(|&(_, b)| b)
    }
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
    /// The context dictionaries read on entry: each one's slot, then the
    /// slot of the dictionary it is read out of and its index there.
    pub(crate) context: Vec<(u32, u32, u32)>,
    /// The `kinds` word, for a shared generic entry or a lambda of one.
    pub(crate) kinds: Option<FrameKinds>,
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
        if dicts.kinds {
            frame.kinds = Some(FrameKinds { slot: Some(frame.frame_base), ..FrameKinds::default() });
            frame.frame_base += 1;
        }
        frame.entry_sp = frame.frame_base;
        if let Some((index, n)) = dicts.kinds_dict
            && let Some(own) = frame.dict_slot(0)
        {
            frame.kinds.get_or_insert_with(FrameKinds::default).dict = Some((own, index, n));
        }
        // Read out of the instance's own dictionary.
        if let Some((base, n)) = dicts.context
            && let Some(own) = frame.dict_slot(0)
        {
            for i in 0..n {
                frame.dict(i + 1);
                let slot = frame.dict_slot(i + 1).expect("context dictionary slot");
                frame.context.push((slot, own, (base + i) as u32));
            }
        }
        frame
    }

    /// A function's frame of one-word parameters and no dictionaries.
    pub(crate) fn plain(hir: &HirBody) -> Self {
        Self::function(hir, |_| None, FrameDicts::default())
    }

    /// A lambda's frame: `lam`'s captures (two words where `pairs` has a
    /// kind), the enclosing body's dictionaries `dicts` it captures and its
    /// `kinds` word, then its parameters.
    pub(crate) fn lambda(lam: &HirBody, pairs: &[Option<PairKind>], dicts: impl IntoIterator<Item = usize>, kinds: Option<&FrameKinds>) -> Self {
        let mut frame = Self::default();
        for (i, &(_, inner)) in lam.captures.iter().enumerate() {
            let local = frame.local(lam.local(inner).name.clone(), pairs.get(i).cloned().flatten());
            frame.captures.push(local);
        }
        for n in dicts {
            frame.dict(n);
        }
        // The enclosing body's `kinds` word, captured after its
        // dictionaries; a lambda checks no sentinel.
        if let Some(outer) = kinds {
            frame.kinds = Some(FrameKinds { slot: Some(frame.frame_base), dict: None, check: None, ..outer.clone() });
            frame.frame_base += 1;
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

    /// The body's entry locals in slot order per kind: a lambda's captures,
    /// then the parameters. A lambda lowers with both as its parameters.
    pub(crate) fn entry_locals(&self) -> impl Iterator<Item = &FrameLocal> {
        self.captures.iter().chain(&self.params)
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
