//! MIR on disk: the native MIR section of a `.ccoil`
//! (`ArchivedProgram::native_mir`).
//!
//! `coil compile` stores the typed MIR of the bodies the native
//! backend compiles, so `coil run` can JIT an archive without the source.
//! This module is the format; `coil_native::section` chooses the bodies. It
//! lives beside MIR, not in the backend, because anything that runs MIR
//! needs it, and the backend is empty on wasm.
//!
//! A full body comes back equal to what went in: every field of
//! [`MirFunc`], every instruction. `COIL_MIR_ARCHIVE_CHECK` runs a corpus
//! that way (`coil_mir_interp::prepare`). Sets and maps are written sorted,
//! so the same MIR gives the same bytes.
//!
//! Layout (little-endian): the magic `CNMIR`, a format byte, the inline
//! scope table its op locations name (`common::write_inline_scopes`, from
//! format 17), then the bodies. Each body is its name, entry ids (`u32::MAX`: none),
//! entry label, then the function. Labels are the compile's own `Label`
//! numbers; a call names its callee by label.

use std::collections::{BTreeSet, HashMap, HashSet};

use common::DebugLoc;

use crate::bodies::Label;
use crate::layout::MirLayout;
use crate::mir::gc::LiveRootSet;
use crate::mir::inst::LocalId;
use crate::mir::{
    BlockId, LaneExpr, LaneVal, MirAllocKind, MirBinOp, MirBlock, MirCastKind, MirCmpOp, MirConst, MirFfiOp, MirFunc, MirGcKind, MirInst, MirLaneShape, MirTy, MirUnaryOp, NativeBody, Terminator, ValueId, VecExpr, VecLoopKind,
};

const MAGIC: &[u8; 5] = common::MIR_SECTION_MAGIC;
/// Bump when the layout changes. A reader takes this format and
/// [`FORMAT_UNSCOPED`], and refuses any other.
const FORMAT: u8 = common::MIR_SECTION_SCOPES_FORMAT;
/// The format before inline scopes: no scope table, no scope in a location
/// (each is `0`). Archives of minor 0 of major 8 have it.
const FORMAT_UNSCOPED: u8 = 16;
const NO_PC: u32 = u32::MAX;

/// The section for `bodies`, each with its entry from `body_ids`, and the
/// inline scopes their locations name.
pub fn encode(bodies: &[NativeBody], body_ids: &HashMap<String, u32>, scopes: &[common::InlineScope]) -> Vec<u8> {
    encode_as(bodies, body_ids, scopes, FORMAT)
}

/// [`encode`] in `format`: [`FORMAT_UNSCOPED`] drops the scopes, as a
/// section of an older archive has none.
fn encode_as(bodies: &[NativeBody], body_ids: &HashMap<String, u32>, scopes: &[common::InlineScope], format: u8) -> Vec<u8> {
    let scoped = format != FORMAT_UNSCOPED;
    let mut w = Writer(Vec::new(), scoped);
    w.0.extend_from_slice(MAGIC);
    w.u8(format);
    if scoped {
        common::write_inline_scopes(&mut w.0, scopes);
    }
    w.u32(bodies.len() as u32);
    for body in bodies {
        w.str(&body.name);
        w.u32(body_ids.get(&body.name).copied().unwrap_or(NO_PC));
        w.u32(body.entry.map_or(u32::MAX, |l| l.0));
        func(&mut w, &body.mir);
    }
    w.0
}


/// The bodies of a section and their body ids.
pub fn decode(bytes: &[u8]) -> Result<(Vec<NativeBody>, HashMap<String, u32>), String> {
    let mut r = Reader { b: bytes, i: 0, scoped: false };
    if r.take(MAGIC.len())? != MAGIC {
        return Err("not a native MIR section".into());
    }
    let format = r.u8()?;
    if format != FORMAT && format != FORMAT_UNSCOPED {
        return Err(format!("native MIR format {format}, this coil reads {FORMAT_UNSCOPED} and {FORMAT}"));
    }
    r.scoped = format == FORMAT;
    if r.scoped {
        common::read_inline_scopes(r.b, &mut r.i).ok_or("native MIR section has a bad inline scope table")?;
    }
    let n = r.u32()?;
    let mut bodies = Vec::new();
    let mut pcs = HashMap::new();
    for _ in 0..n {
        let name = r.str()?;
        let pc = r.u32()?;
        let label = r.u32()?;
        let mir = read_func(&mut r)?;
        if pc != NO_PC {
            pcs.insert(name.clone(), pc);
        }
        let entry = (label != u32::MAX).then_some(Label(label));
        bodies.push(NativeBody { name, entry, mir });
    }
    if r.i != bytes.len() {
        return Err("trailing bytes after the native MIR section".into());
    }
    Ok((bodies, pcs))
}

// ---------------------------------------------------------------------------
// Encoding

const TYS: [MirTy; 8] = [
    MirTy::I64,
    MirTy::F64,
    MirTy::Bool,
    MirTy::HeapRef,
    MirTy::NicheOpt,
    MirTy::NicheRes,
    MirTy::Bottom,
    MirTy::Value,
];
const LAYOUTS: [MirLayout; 3] = [MirLayout::Word, MirLayout::TwoSlot, MirLayout::HeapNiche];
/// Every reason a body refuses emission: the field is a `&'static str`.
const REFUSALS: [&str; 2] = ["no native code", "existential pack"];
const BIN_OPS: [MirBinOp; 11] = [
    MirBinOp::Add,
    MirBinOp::Sub,
    MirBinOp::Mul,
    MirBinOp::Div,
    MirBinOp::Rem,
    MirBinOp::BitAnd,
    MirBinOp::BitOr,
    MirBinOp::Xor,
    MirBinOp::Shl,
    MirBinOp::Shr,
    MirBinOp::Pow,
];
const CMP_OPS: [MirCmpOp; 6] = [MirCmpOp::Lt, MirCmpOp::Le, MirCmpOp::Gt, MirCmpOp::Ge, MirCmpOp::Eq, MirCmpOp::Ne];
const UNARY_OPS: [MirUnaryOp; 2] = [MirUnaryOp::Neg, MirUnaryOp::Not];
const CASTS: [MirCastKind; 3] = [MirCastKind::IntToFloat, MirCastKind::FloatToInt, MirCastKind::Word];

/// Index of `x` in `table`; `0xff`, which [`Reader::pick`] refuses, when
/// it is not there.
fn idx<T: PartialEq>(table: &[T], x: &T) -> u8 {
    table.iter().position(|t| t == x).map_or(0xff, |i| i as u8)
}

const I_CONST_I64: u8 = 0;
const I_CONST_F64: u8 = 1;
const I_CONST_BOOL: u8 = 2;
const I_BIN: u8 = 3;
const I_CMP: u8 = 4;
const I_UNARY: u8 = 5;
const I_CAST: u8 = 6;
const I_PHI: u8 = 7;
const I_CALL: u8 = 8;
const I_INDEX: u8 = 9;
const I_LEN: u8 = 10;
const I_FIELD: u8 = 11;
const I_STORE_INDEX: u8 = 12;
const I_SET_FIELD: u8 = 13;
const I_COW: u8 = 14;
const I_GC_BARRIER: u8 = 15;
const I_ALLOC: u8 = 16;
const I_ARRAY_PUSH: u8 = 17;
const I_MATCH_PAYLOAD: u8 = 18;
const I_STRING: u8 = 19;
const I_CLEAR_WORD: u8 = 20;
const I_STR_EQ: u8 = 21;
const I_STRINGIFY: u8 = 22;
const I_FORMAT: u8 = 23;
const I_FREE_EARLY: u8 = 24;
const I_LOAD_STATIC: u8 = 25;
const I_STORE_STATIC: u8 = 26;
const I_PRINT: u8 = 27;
const I_HOST: u8 = 28;
const I_PANIC: u8 = 29;
const I_CALL_INDIRECT: u8 = 30;
const I_KEEP_ALIVE: u8 = 31;
const I_CODE_PTR: u8 = 32;
const I_HOME_LOAD: u8 = 33;
const I_HOME_STORE: u8 = 34;
const I_RESUME: u8 = 35;
const I_DONE: u8 = 36;
const I_YIELD: u8 = 37;
const I_FFI: u8 = 38;
const I_BYTE_SCAN: u8 = 39;
const I_VEC_LOOP: u8 = 40;
// 41 and 42 are reserved: they were `PanicStore` and `Deopt`, which no
// capture ever made (only tests and the uncalled `promote_fields` built
// them). Never reuse them for another op.
const I_PROBE: u8 = 43;
const I_CALL_INDIRECT_PAIR: u8 = 44;
const I_LANE_INDEX: u8 = 45;
const I_LANE_STORE: u8 = 46;
const I_LANE_PUSH: u8 = 47;
const I_LANES_TO_WORDS: u8 = 48;

const FFI_OPS: [MirFfiOp; 3] = [MirFfiOp::Load, MirFfiOp::Declare, MirFfiOp::Invoke];
/// Deepest lane / vector expression tree a reader takes (a corrupt section
/// cannot recurse without bound).
const MAX_TREE_DEPTH: u32 = 64;

const T_JUMP: u8 = 0;
const T_BR: u8 = 1;
const T_RETURN: u8 = 2;
const T_UNREACHABLE: u8 = 3;
const T_NONE: u8 = 5;
const T_MATCH: u8 = 4;
/// No result type ([`Writer::opt_ty`]), or a return without that word.
const NONE: u8 = 0xff;

/// The bytes, and whether a location has its scope word.
struct Writer(Vec<u8>, bool);

impl Writer {
    fn u8(&mut self, x: u8) {
        self.0.push(x);
    }
    fn u32(&mut self, x: u32) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn u64(&mut self, x: u64) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn loc(&mut self, l: DebugLoc) {
        self.u32(l.file);
        self.u32(l.start_byte);
        self.u32(l.end_byte);
    }
    /// A map keyed by a `u32` id, sorted; then (from format 17) the keys
    /// whose location is in an inline scope, with the scope.
    fn locs(&mut self, locs: impl Iterator<Item = (u32, DebugLoc)>) {
        let mut locs: Vec<_> = locs.collect();
        locs.sort_unstable_by_key(|&(k, _)| k);
        self.u32(locs.len() as u32);
        for &(k, l) in &locs {
            self.u32(k);
            self.loc(l);
        }
        if self.1 {
            let scoped: Vec<(u32, u32)> = locs.iter().filter(|(_, l)| l.scope != 0).map(|&(k, l)| (k, l.scope)).collect();
            self.u32(scoped.len() as u32);
            for (k, scope) in scoped {
                self.u32(k);
                self.u32(scope);
            }
        }
    }
    fn locals(&mut self, locals: impl ExactSizeIterator<Item = LocalId>) {
        self.u32(locals.len() as u32);
        for l in locals {
            self.u32(l.0);
        }
    }
    fn binds(&mut self, binds: &[(LocalId, ValueId)]) {
        self.u32(binds.len() as u32);
        for &(l, v) in binds {
            self.u32(l.0);
            self.v(v);
        }
    }
    fn value_set(&mut self, set: &HashSet<ValueId>) {
        let mut vs: Vec<ValueId> = set.iter().copied().collect();
        vs.sort_unstable_by_key(|v| v.0);
        self.vs(&vs);
    }
    fn ty(&mut self, t: MirTy) {
        self.u8(idx(&TYS, &t));
    }
    fn opt_ty(&mut self, t: Option<MirTy>) {
        match t {
            Some(t) => self.ty(t),
            None => self.u8(NONE),
        }
    }
    fn opt_v(&mut self, v: Option<ValueId>) {
        match v {
            Some(v) => {
                self.u8(1);
                self.v(v);
            }
            None => self.u8(0),
        }
    }
    fn v(&mut self, v: ValueId) {
        self.u32(v.0);
    }
    fn vs(&mut self, vs: &[ValueId]) {
        self.u32(vs.len() as u32);
        for &v in vs {
            self.v(v);
        }
    }
}

fn func(w: &mut Writer, f: &MirFunc) {
    w.str(&f.name);
    w.u32(f.entry.0);
    w.u32(f.types.len() as u32);
    for &t in &f.types {
        w.ty(t);
    }
    w.vs(&f.params);
    w.opt_ty(f.ret_ty);
    w.opt_ty(f.ret_hi_ty);
    w.u8(idx(&LAYOUTS, &f.ret_layout));
    w.u32(f.blocks.len() as u32);
    for b in &f.blocks {
        w.u32(b.id.0);
        w.u32(b.insts.len() as u32);
        for inst in &b.insts {
            self::inst(w, inst);
        }
        match &b.term {
            Some(Terminator::Jump { dest }) => {
                w.u8(T_JUMP);
                w.u32(dest.0);
            }
            Some(Terminator::Br { cond, taken, not_taken }) => {
                w.u8(T_BR);
                w.v(*cond);
                w.u32(taken.0);
                w.u32(not_taken.0);
            }
            Some(Terminator::Return { lo, hi }) => {
                w.u8(T_RETURN);
                w.opt_v(*lo);
                w.opt_v(*hi);
            }
            Some(Terminator::JumpIfMatch { scrutinee, tag, payloads, taken, not_taken }) => {
                w.u8(T_MATCH);
                w.v(*scrutinee);
                w.u32(*tag);
                w.u32(payloads.len() as u32);
                for p in payloads {
                    w.v(*p);
                }
                w.u32(taken.0);
                w.u32(not_taken.0);
            }
            Some(Terminator::Unreachable) => w.u8(T_UNREACHABLE),
            None => w.u8(T_NONE),
        }
    }
    w.locs(f.value_locs.iter().map(|(v, l)| (v.0, *l)));
    w.locs(f.op_locs.iter().map(|(v, l)| (v.0, *l)));
    w.locs(f.term_locs.iter().map(|(b, l)| (b.0, *l)));
    // The tables the root analyses and the passes read.
    let mut slot_env: Vec<_> = f.slot_env.iter().collect();
    slot_env.sort_unstable_by_key(|(v, _)| v.0);
    w.u32(slot_env.len() as u32);
    for (v, binds) in slot_env {
        w.v(*v);
        w.binds(binds);
    }
    w.u32(f.gc_roots.len() as u32);
    for set in &f.gc_roots {
        w.v(set.at);
        w.vs(&set.values);
        w.locals(set.slots.iter().copied());
    }
    let mut debug_slots: Vec<_> = f.debug_slots.iter().map(|(&l, &v)| (l, v)).collect();
    debug_slots.sort_unstable_by_key(|(l, _)| l.0);
    w.binds(&debug_slots);
    w.locals(f.heap_slots.iter().copied());
    w.locals(f.word_slots.iter().copied());
    w.binds(&f.local_binds);
    w.u8(f.emit_refusal.map_or(NONE, |why| {
        let at = REFUSALS.iter().position(|r| *r == why);
        at.expect("a refusal reason the format lists") as u8
    }));
    let flags = [f.debug_keep, f.keep_lines, f.opaque_words, f.lengths_steady];
    w.u8(flags.iter().enumerate().fold(0, |bits, (i, &on)| bits | (u8::from(on) << i)));
    let mut tail_targets: Vec<u32> = f.tail_targets.iter().map(|l| l.0).collect();
    tail_targets.sort_unstable();
    w.u32(tail_targets.len() as u32);
    for t in tail_targets {
        w.u32(t);
    }
    for set in [
        &f.heap_pair_words,
        &f.pure_calls,
        &f.invariant_loads,
        &f.byte_loads,
        &f.prefetch_loads,
        &f.steady_calls,
        &f.bound_words,
    ] {
        w.value_set(set);
    }
    // `defer` homes, and the cleanup pad that reads them.
    w.u32(f.homes.len() as u32);
    for &t in &f.homes {
        w.ty(t);
    }
    w.u8(u8::from(f.home_base.is_some()));
    w.u8(f.home_base.unwrap_or(0));
    match f.cleanup.as_deref() {
        Some(pad) => {
            w.u8(1);
            func(w, pad);
        }
        None => w.u8(0),
    }
    w.u32(f.pair_twin.map_or(u32::MAX, |l| l.0));
}

fn inst(w: &mut Writer, inst: &MirInst) {
    match inst {
        MirInst::Const { dest, c } => {
            match c {
                MirConst::I64(x) => {
                    w.u8(I_CONST_I64);
                    w.u64(*x as u64);
                }
                MirConst::F64(bits) => {
                    w.u8(I_CONST_F64);
                    w.u64(*bits);
                }
                MirConst::Bool(b) => {
                    w.u8(I_CONST_BOOL);
                    w.u64(*b as u64);
                }
            }
            w.v(*dest);
        }
        MirInst::Bin { dest, op, ty, lhs, rhs } => {
            w.u8(I_BIN);
            w.u8(idx(&BIN_OPS, op));
            w.ty(*ty);
            w.v(*dest);
            w.v(*lhs);
            w.v(*rhs);
        }
        MirInst::Cmp { dest, op, ty, lhs, rhs } => {
            w.u8(I_CMP);
            w.u8(idx(&CMP_OPS, op));
            w.ty(*ty);
            w.v(*dest);
            w.v(*lhs);
            w.v(*rhs);
        }
        MirInst::Unary { dest, op, src } => {
            w.u8(I_UNARY);
            w.u8(idx(&UNARY_OPS, op));
            w.v(*dest);
            w.v(*src);
        }
        MirInst::Cast { dest, kind, to, src } => {
            w.u8(I_CAST);
            w.u8(idx(&CASTS, kind));
            w.ty(*to);
            w.v(*dest);
            w.v(*src);
        }
        MirInst::Phi { dest, ty, args } => {
            w.u8(I_PHI);
            w.ty(*ty);
            w.v(*dest);
            w.u32(args.len() as u32);
            for (b, v) in args {
                w.u32(b.0);
                w.v(*v);
            }
        }
        MirInst::Call { dest, dest_hi, target, args } => {
            w.u8(I_CALL);
            w.v(*dest);
            w.opt_v(*dest_hi);
            w.u32(target.0);
            w.vs(args);
        }
        MirInst::Index { dest, array, index, unchecked } => {
            w.u8(I_INDEX);
            w.v(*dest);
            w.v(*array);
            w.v(*index);
            w.u8(*unchecked as u8);
        }
        MirInst::LaneIndex { dest, dest_hi, array, index } => {
            w.u8(I_LANE_INDEX);
            w.v(*dest);
            w.v(*dest_hi);
            w.v(*array);
            w.v(*index);
        }
        MirInst::LaneStore { dest, array, index, lo, hi, shape } => {
            w.u8(I_LANE_STORE);
            w.v(*dest);
            w.v(*array);
            w.v(*index);
            w.v(*lo);
            w.v(*hi);
            lane_shape(w, shape);
        }
        MirInst::LanePush { dest, array, lo, hi, shape } => {
            w.u8(I_LANE_PUSH);
            w.v(*dest);
            w.v(*array);
            w.v(*lo);
            w.v(*hi);
            lane_shape(w, shape);
        }
        MirInst::LanesToWords { dest, array, shape } => {
            w.u8(I_LANES_TO_WORDS);
            w.v(*dest);
            w.v(*array);
            lane_shape(w, shape);
        }
        MirInst::ArrayLen { dest, array } => {
            w.u8(I_LEN);
            w.v(*dest);
            w.v(*array);
        }
        MirInst::ProductLoad { dest, object, slot } => {
            w.u8(I_FIELD);
            w.v(*dest);
            w.v(*object);
            w.u32(*slot);
        }
        MirInst::StoreIndex { dest, array, index, value, unchecked } => {
            w.u8(I_STORE_INDEX);
            w.v(*dest);
            w.v(*array);
            w.v(*index);
            w.v(*value);
            w.u8(*unchecked as u8);
        }
        MirInst::HeapFieldStore { dest, object, value, index } => {
            w.u8(I_SET_FIELD);
            w.v(*dest);
            w.v(*object);
            w.v(*value);
            w.u32(*index);
        }
        MirInst::Cow { dest, src, unique, field } => {
            w.u8(I_COW);
            w.v(*dest);
            w.v(*src);
            w.u8(*unique as u8);
            w.u32(field.unwrap_or(u32::MAX));
        }
        MirInst::Alloc { dest, kind, elems, kinds } => {
            w.u8(I_ALLOC);
            w.v(*dest);
            match *kind {
                MirAllocKind::Array => w.u8(0),
                MirAllocKind::Lanes { tag_mask } => {
                    w.u8(9);
                    w.u64(tag_mask);
                }
                MirAllocKind::Enum { tag } => {
                    w.u8(2);
                    w.u32(tag);
                }
                MirAllocKind::Object { type_id, nfields } => {
                    w.u8(3);
                    w.u32(type_id);
                    w.u32(nfields);
                }
                MirAllocKind::Fn { target, operand, mask } => {
                    w.u8(4);
                    w.u32(target.0);
                    w.u32(operand);
                    w.u32(mask);
                }
                MirAllocKind::DropEnum { tag, type_id } => {
                    w.u8(7);
                    w.u32(tag);
                    w.u32(type_id);
                }
                MirAllocKind::PolyFn { target } => {
                    w.u8(5);
                    w.u32(target.0);
                }
                MirAllocKind::PolyFnCapture { target, params } => {
                    w.u8(6);
                    w.u32(target.0);
                    w.u32(params);
                }
                MirAllocKind::Coro { target } => {
                    w.u8(8);
                    w.u32(target.0);
                }
            }
            w.u32(elems.len() as u32);
            for e in elems {
                w.v(*e);
            }
            w.u8(*kinds);
        }
        MirInst::ArrayPush { dest, array, value } => {
            w.u8(I_ARRAY_PUSH);
            w.v(*dest);
            w.v(*array);
            w.v(*value);
        }
        MirInst::String { dest, idx } => {
            w.u8(I_STRING);
            w.v(*dest);
            w.u32(*idx);
        }
        MirInst::LoadStatic { dest, slot } => {
            w.u8(I_LOAD_STATIC);
            w.v(*dest);
            w.u32(*slot);
        }
        MirInst::StoreStatic { dest, slot, value } => {
            w.u8(I_STORE_STATIC);
            w.v(*dest);
            w.u32(*slot);
            w.v(*value);
        }
        MirInst::HostInvoke { dest, dest_hi, native_id, layout, args } => {
            w.u8(I_HOST);
            w.v(*dest);
            w.opt_v(*dest_hi);
            w.u32(u32::from(*native_id));
            w.u8(*layout);
            w.vs(args);
        }
        MirInst::CodePtr { dest, target } => {
            w.u8(I_CODE_PTR);
            w.v(*dest);
            w.u32(target.0);
        }
        MirInst::HomeLoad { dest, home } => {
            w.u8(I_HOME_LOAD);
            w.v(*dest);
            w.u32(u32::from(*home));
        }
        MirInst::HomeStore { dest, home, value } => {
            w.u8(I_HOME_STORE);
            w.v(*dest);
            w.u32(u32::from(*home));
            w.v(*value);
        }
        MirInst::KeepAlive { dest, value } => {
            w.u8(I_KEEP_ALIVE);
            w.v(*dest);
            w.v(*value);
        }
        MirInst::Probe { dest, id } => {
            w.u8(I_PROBE);
            w.v(*dest);
            w.u32(*id);
        }
        MirInst::CallIndirect { dest, callee, args, dicts, pair } => {
            w.u8(if pair.is_some() { I_CALL_INDIRECT_PAIR } else { I_CALL_INDIRECT });
            if let Some((hi, unpair)) = pair {
                w.v(*hi);
                w.u32(unpair.0);
            }
            w.v(*dest);
            w.v(*callee);
            w.vs(args);
            w.u32(*dicts);
        }
        MirInst::Panic { dest, msg } => {
            w.u8(I_PANIC);
            w.v(*dest);
            w.v(*msg);
        }
        MirInst::Print { dest, src } => {
            w.u8(I_PRINT);
            w.v(*dest);
            w.v(*src);
        }
        MirInst::FreeEarly { dest, value, deep } => {
            w.u8(I_FREE_EARLY);
            w.v(*dest);
            w.v(*value);
            w.u8(*deep as u8);
        }
        MirInst::Format { dest, fmt, args } => {
            w.u8(I_FORMAT);
            w.v(*dest);
            w.v(*fmt);
            w.vs(args);
        }
        MirInst::Stringify { dest, src, tag } => {
            w.u8(I_STRINGIFY);
            w.v(*dest);
            w.v(*src);
            w.u32(tag.unwrap_or(u32::MAX));
        }
        MirInst::StrEq { dest, lhs, rhs, ne } => {
            w.u8(I_STR_EQ);
            w.v(*dest);
            w.v(*lhs);
            w.v(*rhs);
            w.u8(*ne as u8);
        }
        MirInst::ClearWord { dest, value } => {
            w.u8(I_CLEAR_WORD);
            w.v(*dest);
            w.v(*value);
        }
        MirInst::MatchPayload { dest, scrutinee, index } => {
            w.u8(I_MATCH_PAYLOAD);
            w.v(*dest);
            w.v(*scrutinee);
            w.u32(*index);
        }
        MirInst::GcBarrier { dest, kind: MirGcKind::Safepoint, roots } => {
            w.u8(I_GC_BARRIER);
            w.v(*dest);
            w.vs(roots);
        }
        MirInst::ResumeCoro { dest, handle, send } => {
            w.u8(I_RESUME);
            w.v(*dest);
            w.v(*handle);
            w.opt_v(*send);
        }
        MirInst::DoneCoro { dest, handle } => {
            w.u8(I_DONE);
            w.v(*dest);
            w.v(*handle);
        }
        MirInst::Yield { dest, value, from, receive } => {
            w.u8(I_YIELD);
            w.v(*dest);
            w.v(*value);
            w.u8(u8::from(*from));
            w.u8(u8::from(*receive));
        }
        MirInst::Ffi { dest, op, operand, args } => {
            w.u8(I_FFI);
            w.u8(idx(&FFI_OPS, op));
            w.u32(*operand);
            w.v(*dest);
            w.vs(args);
        }
        MirInst::ByteScan { dest, from, bound, inclusive, backward, streams, scalars, expr } => {
            w.u8(I_BYTE_SCAN);
            w.v(*dest);
            w.v(*from);
            w.v(*bound);
            w.u8(u8::from(*inclusive) | u8::from(*backward) << 1);
            w.u32(streams.len() as u32);
            for &(a, i) in streams {
                w.v(a);
                w.v(i);
            }
            w.vs(scalars);
            lane_expr(w, expr);
        }
        MirInst::VecLoop { dest, pos, from, to, kind } => {
            w.u8(I_VEC_LOOP);
            w.v(*dest);
            w.opt_v(*pos);
            w.v(*from);
            w.v(*to);
            match kind {
                VecLoopKind::Reduce { init, ty, fold, term, stores } => {
                    w.u8(0);
                    w.v(*init);
                    w.u8(*ty);
                    w.u8(*fold);
                    vec_expr(w, term);
                    vec_stores(w, stores);
                }
                VecLoopKind::Store { stores } => {
                    w.u8(1);
                    vec_stores(w, stores);
                }
                VecLoopKind::Push { out, ty, value } => {
                    w.u8(2);
                    w.v(*out);
                    w.u8(*ty);
                    vec_expr(w, value);
                }
            }
        }
    }
}

fn lane_shape(w: &mut Writer, s: &MirLaneShape) {
    w.u64(s.tag_mask);
    w.u64(s.payload_mask);
    w.u64(s.unit_mask);
}

fn lane_val(w: &mut Writer, v: LaneVal) {
    match v {
        LaneVal::Stream(s) => {
            w.u8(0);
            w.u8(s);
        }
        LaneVal::Scalar(k) => {
            w.u8(1);
            w.u8(k);
        }
    }
}

fn lane_expr(w: &mut Writer, e: &LaneExpr) {
    match e {
        LaneExpr::Const(b) => {
            w.u8(0);
            w.u8(u8::from(*b));
        }
        LaneExpr::Cmp { op, lhs, rhs } => {
            w.u8(1);
            w.u8(idx(&CMP_OPS, op));
            lane_val(w, *lhs);
            lane_val(w, *rhs);
        }
        LaneExpr::And(a, b) | LaneExpr::Or(a, b) => {
            w.u8(if matches!(e, LaneExpr::And(..)) { 2 } else { 3 });
            lane_expr(w, a);
            lane_expr(w, b);
        }
        LaneExpr::Not(a) => {
            w.u8(4);
            lane_expr(w, a);
        }
    }
}

fn vec_expr(w: &mut Writer, e: &VecExpr) {
    match e {
        VecExpr::Splat { v, ty } => {
            w.u8(0);
            w.v(*v);
            w.u8(*ty);
        }
        VecExpr::Index => w.u8(1),
        VecExpr::Load { array, off, ty } => {
            w.u8(2);
            w.v(*array);
            w.v(*off);
            w.u8(*ty);
        }
        VecExpr::Bin { kind, lhs, rhs } => {
            w.u8(3);
            w.u8(*kind);
            vec_expr(w, lhs);
            vec_expr(w, rhs);
        }
        VecExpr::Unary { kind, src } => {
            w.u8(4);
            w.u8(*kind);
            vec_expr(w, src);
        }
        VecExpr::Select { mask, then, els } => {
            w.u8(5);
            vec_expr(w, mask);
            vec_expr(w, then);
            vec_expr(w, els);
        }
        VecExpr::Fma { ty, a, b, c } => {
            w.u8(6);
            w.u8(*ty);
            vec_expr(w, a);
            vec_expr(w, b);
            vec_expr(w, c);
        }
    }
}

fn vec_stores(w: &mut Writer, stores: &[(ValueId, ValueId, u8, VecExpr)]) {
    w.u32(stores.len() as u32);
    for (array, off, ty, value) in stores {
        w.v(*array);
        w.v(*off);
        w.u8(*ty);
        vec_expr(w, value);
    }
}

// ---------------------------------------------------------------------------
// Decoding

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
    /// The section's locations have a scope word (format 17 on).
    scoped: bool,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.i.checked_add(n).filter(|&e| e <= self.b.len()).ok_or("native MIR section is truncated")?;
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// A length that cannot exceed what is left of the section (each
    /// element takes at least one byte), so a corrupt count cannot ask
    /// for a huge allocation.
    fn len(&mut self) -> Result<usize, String> {
        let n = self.u32()? as usize;
        if n > self.b.len() - self.i {
            return Err("native MIR section has a bad length".into());
        }
        Ok(n)
    }
    fn str(&mut self) -> Result<String, String> {
        let n = self.len()?;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "native MIR name is not UTF-8".into())
    }
    fn loc(&mut self) -> Result<DebugLoc, String> {
        Ok(DebugLoc::span(self.u32()?, self.u32()?, self.u32()?))
    }
    fn locs(&mut self) -> Result<Vec<(u32, DebugLoc)>, String> {
        let n = self.len()?;
        let mut locs = (0..n).map(|_| Ok((self.u32()?, self.loc()?))).collect::<Result<Vec<_>, String>>()?;
        if self.scoped {
            for _ in 0..self.len()? {
                let (k, scope) = (self.u32()?, self.u32()?);
                // Keys are written sorted.
                let at = locs.binary_search_by_key(&k, |&(key, _)| key).map_err(|_| "native MIR section scopes an unknown location")?;
                locs[at].1.scope = scope;
            }
        }
        Ok(locs)
    }
    fn locals(&mut self) -> Result<Vec<LocalId>, String> {
        let n = self.len()?;
        (0..n).map(|_| self.u32().map(LocalId)).collect()
    }
    fn binds(&mut self) -> Result<Vec<(LocalId, ValueId)>, String> {
        let n = self.len()?;
        (0..n).map(|_| Ok((LocalId(self.u32()?), self.v()?))).collect()
    }
    fn value_set(&mut self) -> Result<HashSet<ValueId>, String> {
        Ok(self.vs()?.into_iter().collect())
    }
    fn pick<T: Clone>(&mut self, table: &[T], what: &str) -> Result<T, String> {
        let i = self.u8()?;
        table.get(i as usize).cloned().ok_or_else(|| format!("native MIR: bad {what} {i}"))
    }
    fn ty(&mut self) -> Result<MirTy, String> {
        self.pick(&TYS, "type")
    }
    fn opt_ty(&mut self) -> Result<Option<MirTy>, String> {
        if self.b.get(self.i) == Some(&NONE) {
            self.i += 1;
            return Ok(None);
        }
        self.ty().map(Some)
    }
    fn opt_v(&mut self) -> Result<Option<ValueId>, String> {
        match self.u8()? {
            0 => Ok(None),
            1 => self.v().map(Some),
            t => Err(format!("native MIR: bad optional value {t}")),
        }
    }
    fn v(&mut self) -> Result<ValueId, String> {
        Ok(ValueId(self.u32()?))
    }
    fn vs(&mut self) -> Result<Vec<ValueId>, String> {
        let n = self.len()?;
        (0..n).map(|_| self.v()).collect()
    }
}

fn read_func(r: &mut Reader) -> Result<MirFunc, String> {
    let name = r.str()?;
    let mut f = MirFunc::new(&name);
    f.blocks.clear();
    f.entry = BlockId(r.u32()?);
    let n = r.len()?;
    f.types = (0..n).map(|_| r.ty()).collect::<Result<_, _>>()?;
    f.params = r.vs()?;
    f.ret_ty = r.opt_ty()?;
    f.ret_hi_ty = r.opt_ty()?;
    f.ret_layout = r.pick(&LAYOUTS, "layout")?;
    let nb = r.len()?;
    for _ in 0..nb {
        let mut b = MirBlock::new(BlockId(r.u32()?));
        let ni = r.len()?;
        for _ in 0..ni {
            b.insts.push(read_inst(r)?);
        }
        b.term = match r.u8()? {
            T_JUMP => Some(Terminator::Jump { dest: BlockId(r.u32()?) }),
            T_BR => Some(Terminator::Br { cond: r.v()?, taken: BlockId(r.u32()?), not_taken: BlockId(r.u32()?) }),
            T_RETURN => Some(Terminator::Return { lo: r.opt_v()?, hi: r.opt_v()? }),
            T_UNREACHABLE => Some(Terminator::Unreachable),
            T_MATCH => {
                let (scrutinee, tag) = (r.v()?, r.u32()?);
                let n = r.len()?;
                let payloads = (0..n).map(|_| r.v()).collect::<Result<Vec<_>, _>>()?;
                Some(Terminator::JumpIfMatch { scrutinee, tag, payloads, taken: BlockId(r.u32()?), not_taken: BlockId(r.u32()?) })
            }
            T_NONE => None,
            t => return Err(format!("native MIR: bad terminator {t}")),
        };
        f.blocks.push(b);
    }
    f.value_locs = r.locs()?.into_iter().map(|(k, l)| (ValueId(k), l)).collect();
    f.op_locs = r.locs()?.into_iter().map(|(k, l)| (ValueId(k), l)).collect();
    f.term_locs = r.locs()?.into_iter().map(|(k, l)| (BlockId(k), l)).collect();
    let n = r.len()?;
    for _ in 0..n {
        let v = r.v()?;
        f.slot_env.insert(v, r.binds()?);
    }
    let n = r.len()?;
    for _ in 0..n {
        f.gc_roots.push(LiveRootSet { at: r.v()?, values: r.vs()?, slots: r.locals()? });
    }
    f.debug_slots = r.binds()?.into_iter().collect();
    f.heap_slots = r.locals()?.into_iter().collect::<BTreeSet<_>>();
    f.word_slots = r.locals()?.into_iter().collect::<BTreeSet<_>>();
    f.local_binds = r.binds()?;
    f.emit_refusal = match r.u8()? {
        NONE => None,
        i => Some(*REFUSALS.get(i as usize).ok_or_else(|| format!("native MIR: bad refusal {i}"))?),
    };
    let flags = r.u8()?;
    f.debug_keep = flags & 1 != 0;
    f.keep_lines = flags & 2 != 0;
    f.opaque_words = flags & 4 != 0;
    f.lengths_steady = flags & 8 != 0;
    let n = r.len()?;
    f.tail_targets = (0..n).map(|_| r.u32().map(Label)).collect::<Result<_, _>>()?;
    f.heap_pair_words = r.value_set()?;
    f.pure_calls = r.value_set()?;
    f.invariant_loads = r.value_set()?;
    f.byte_loads = r.value_set()?;
    f.prefetch_loads = r.value_set()?;
    f.steady_calls = r.value_set()?;
    f.bound_words = r.value_set()?;
    let nh = r.len()?;
    f.homes = (0..nh).map(|_| r.ty()).collect::<Result<_, _>>()?;
    let has_base = r.u8()? != 0;
    let base = r.u8()?;
    f.home_base = has_base.then_some(base);
    if r.u8()? == 1 {
        f.cleanup = Some(Box::new(read_func(r)?));
    }
    f.pair_twin = Some(r.u32()?).filter(|&l| l != u32::MAX).map(Label);
    Ok(f)
}

fn read_inst(r: &mut Reader) -> Result<MirInst, String> {
    Ok(match r.u8()? {
        I_CONST_I64 => {
            let c = MirConst::I64(r.u64()? as i64);
            MirInst::Const { c, dest: r.v()? }
        }
        I_CONST_F64 => {
            let c = MirConst::F64(r.u64()?);
            MirInst::Const { c, dest: r.v()? }
        }
        I_CONST_BOOL => {
            let c = MirConst::Bool(r.u64()? != 0);
            MirInst::Const { c, dest: r.v()? }
        }
        I_BIN => {
            let op = r.pick(&BIN_OPS, "binary op")?;
            let ty = r.ty()?;
            MirInst::Bin { op, ty, dest: r.v()?, lhs: r.v()?, rhs: r.v()? }
        }
        I_CMP => {
            let op = r.pick(&CMP_OPS, "compare op")?;
            let ty = r.ty()?;
            MirInst::Cmp { op, ty, dest: r.v()?, lhs: r.v()?, rhs: r.v()? }
        }
        I_UNARY => {
            let op = r.pick(&UNARY_OPS, "unary op")?;
            MirInst::Unary { op, dest: r.v()?, src: r.v()? }
        }
        I_CAST => {
            let kind = r.pick(&CASTS, "cast")?;
            let to = r.ty()?;
            MirInst::Cast { kind, to, dest: r.v()?, src: r.v()? }
        }
        I_PHI => {
            let ty = r.ty()?;
            let dest = r.v()?;
            let n = r.len()?;
            let args = (0..n).map(|_| Ok((BlockId(r.u32()?), r.v()?))).collect::<Result<_, String>>()?;
            MirInst::Phi { dest, ty, args }
        }
        I_CALL => {
            let (dest, dest_hi) = (r.v()?, r.opt_v()?);
            let target = Label(r.u32()?);
            MirInst::Call { dest, dest_hi, target, args: r.vs()? }
        }
        I_INDEX => MirInst::Index { dest: r.v()?, array: r.v()?, index: r.v()?, unchecked: r.u8()? != 0 },
        I_LEN => MirInst::ArrayLen { dest: r.v()?, array: r.v()? },
        I_FIELD => MirInst::ProductLoad { dest: r.v()?, object: r.v()?, slot: r.u32()? },
        I_STORE_INDEX => MirInst::StoreIndex {
            dest: r.v()?,
            array: r.v()?,
            index: r.v()?,
            value: r.v()?,
            unchecked: r.u8()? != 0,
        },
        I_SET_FIELD => MirInst::HeapFieldStore { dest: r.v()?, object: r.v()?, value: r.v()?, index: r.u32()? },
        I_ALLOC => {
            let dest = r.v()?;
            let kind = match r.u8()? {
                0 => MirAllocKind::Array,
                2 => MirAllocKind::Enum { tag: r.u32()? },
                3 => MirAllocKind::Object { type_id: r.u32()?, nfields: r.u32()? },
                4 => MirAllocKind::Fn { target: Label(r.u32()?), operand: r.u32()?, mask: r.u32()? },
                7 => MirAllocKind::DropEnum { tag: r.u32()?, type_id: r.u32()? },
                5 => MirAllocKind::PolyFn { target: Label(r.u32()?) },
                6 => MirAllocKind::PolyFnCapture { target: Label(r.u32()?), params: r.u32()? },
                8 => MirAllocKind::Coro { target: Label(r.u32()?) },
                9 => MirAllocKind::Lanes { tag_mask: r.u64()? },
                k => return Err(format!("native MIR: bad allocation kind {k}")),
            };
            let n = r.len()?;
            let elems = (0..n).map(|_| r.v()).collect::<Result<Vec<_>, _>>()?;
            MirInst::Alloc { dest, kind, elems, kinds: r.u8()? }
        }
        I_STRING => MirInst::String { dest: r.v()?, idx: r.u32()? },
        I_LOAD_STATIC => MirInst::LoadStatic { dest: r.v()?, slot: r.u32()? },
        I_STORE_STATIC => MirInst::StoreStatic { dest: r.v()?, slot: r.u32()?, value: r.v()? },
        I_PRINT => MirInst::Print { dest: r.v()?, src: r.v()? },
        I_PANIC => MirInst::Panic { dest: r.v()?, msg: r.v()? },
        I_CALL_INDIRECT => MirInst::CallIndirect { dest: r.v()?, callee: r.v()?, args: r.vs()?, dicts: r.u32()?, pair: None },
        I_CALL_INDIRECT_PAIR => {
            let pair = Some((r.v()?, Label(r.u32()?)));
            MirInst::CallIndirect { dest: r.v()?, callee: r.v()?, args: r.vs()?, dicts: r.u32()?, pair }
        }
        I_HOST => MirInst::HostInvoke {
            dest: r.v()?,
            dest_hi: r.opt_v()?,
            native_id: u16::try_from(r.u32()?).map_err(|_| "native MIR: bad host id")?,
            layout: r.u8()?,
            args: r.vs()?,
        },
        I_FREE_EARLY => MirInst::FreeEarly { dest: r.v()?, value: r.v()?, deep: r.u8()? != 0 },
        I_FORMAT => MirInst::Format { dest: r.v()?, fmt: r.v()?, args: r.vs()? },
        I_STRINGIFY => MirInst::Stringify { dest: r.v()?, src: r.v()?, tag: Some(r.u32()?).filter(|&t| t != u32::MAX) },
        I_STR_EQ => MirInst::StrEq { dest: r.v()?, lhs: r.v()?, rhs: r.v()?, ne: r.u8()? != 0 },
        I_CLEAR_WORD => MirInst::ClearWord { dest: r.v()?, value: r.v()? },
        I_KEEP_ALIVE => MirInst::KeepAlive { dest: r.v()?, value: r.v()? },
        I_PROBE => MirInst::Probe { dest: r.v()?, id: r.u32()? },
        I_CODE_PTR => MirInst::CodePtr { dest: r.v()?, target: Label(r.u32()?) },
        I_HOME_LOAD => MirInst::HomeLoad { dest: r.v()?, home: u16::try_from(r.u32()?).map_err(|_| "native MIR: bad home")? },
        I_HOME_STORE => MirInst::HomeStore {
            dest: r.v()?,
            home: u16::try_from(r.u32()?).map_err(|_| "native MIR: bad home")?,
            value: r.v()?,
        },
        I_MATCH_PAYLOAD => MirInst::MatchPayload { dest: r.v()?, scrutinee: r.v()?, index: r.u32()? },
        I_ARRAY_PUSH => MirInst::ArrayPush { dest: r.v()?, array: r.v()?, value: r.v()? },
        I_LANE_INDEX => MirInst::LaneIndex { dest: r.v()?, dest_hi: r.v()?, array: r.v()?, index: r.v()? },
        I_LANE_STORE => MirInst::LaneStore { dest: r.v()?, array: r.v()?, index: r.v()?, lo: r.v()?, hi: r.v()?, shape: read_lane_shape(r)? },
        I_LANE_PUSH => MirInst::LanePush { dest: r.v()?, array: r.v()?, lo: r.v()?, hi: r.v()?, shape: read_lane_shape(r)? },
        I_LANES_TO_WORDS => MirInst::LanesToWords { dest: r.v()?, array: r.v()?, shape: read_lane_shape(r)? },
        I_GC_BARRIER => MirInst::GcBarrier { dest: r.v()?, kind: MirGcKind::Safepoint, roots: r.vs()? },
        I_COW => {
            let (dest, src, unique) = (r.v()?, r.v()?, r.u8()? != 0);
            let field = Some(r.u32()?).filter(|&f| f != u32::MAX);
            MirInst::Cow { dest, src, unique, field }
        }
        I_RESUME => MirInst::ResumeCoro { dest: r.v()?, handle: r.v()?, send: r.opt_v()? },
        I_DONE => MirInst::DoneCoro { dest: r.v()?, handle: r.v()? },
        I_YIELD => MirInst::Yield { dest: r.v()?, value: r.v()?, from: r.u8()? != 0, receive: r.u8()? != 0 },
        I_FFI => {
            let (op, operand) = (r.pick(&FFI_OPS, "FFI op")?, r.u32()?);
            MirInst::Ffi { op, operand, dest: r.v()?, args: r.vs()? }
        }
        I_BYTE_SCAN => {
            let (dest, from, bound, flags) = (r.v()?, r.v()?, r.v()?, r.u8()?);
            let n = r.len()?;
            let streams = (0..n).map(|_| Ok((r.v()?, r.v()?))).collect::<Result<Vec<_>, String>>()?;
            let scalars = r.vs()?;
            let expr = read_lane_expr(r, 0)?;
            MirInst::ByteScan { dest, from, bound, inclusive: flags & 1 != 0, backward: flags & 2 != 0, streams, scalars, expr }
        }
        I_VEC_LOOP => {
            let (dest, pos, from, to) = (r.v()?, r.opt_v()?, r.v()?, r.v()?);
            let kind = match r.u8()? {
                0 => {
                    let (init, ty, fold) = (r.v()?, r.u8()?, r.u8()?);
                    let term = read_vec_expr(r, 0)?;
                    VecLoopKind::Reduce { init, ty, fold, term, stores: read_vec_stores(r)? }
                }
                1 => VecLoopKind::Store { stores: read_vec_stores(r)? },
                2 => {
                    let (out, ty) = (r.v()?, r.u8()?);
                    VecLoopKind::Push { out, ty, value: read_vec_expr(r, 0)? }
                }
                k => return Err(format!("native MIR: bad vector loop {k}")),
            };
            MirInst::VecLoop { dest, pos, from, to, kind }
        }
        t => return Err(format!("native MIR: bad instruction {t}")),
    })
}

fn read_lane_shape(r: &mut Reader) -> Result<MirLaneShape, String> {
    Ok(MirLaneShape { tag_mask: r.u64()?, payload_mask: r.u64()?, unit_mask: r.u64()? })
}

fn read_lane_val(r: &mut Reader) -> Result<LaneVal, String> {
    Ok(match r.u8()? {
        0 => LaneVal::Stream(r.u8()?),
        1 => LaneVal::Scalar(r.u8()?),
        t => return Err(format!("native MIR: bad lane value {t}")),
    })
}

fn read_lane_expr(r: &mut Reader, depth: u32) -> Result<LaneExpr, String> {
    if depth > MAX_TREE_DEPTH {
        return Err("native MIR: lane expression too deep".into());
    }
    let sub = |r: &mut Reader| read_lane_expr(r, depth + 1).map(Box::new);
    Ok(match r.u8()? {
        0 => LaneExpr::Const(r.u8()? != 0),
        1 => LaneExpr::Cmp { op: r.pick(&CMP_OPS, "compare")?, lhs: read_lane_val(r)?, rhs: read_lane_val(r)? },
        2 => LaneExpr::And(sub(r)?, sub(r)?),
        3 => LaneExpr::Or(sub(r)?, sub(r)?),
        4 => LaneExpr::Not(sub(r)?),
        t => return Err(format!("native MIR: bad lane expression {t}")),
    })
}

fn read_vec_expr(r: &mut Reader, depth: u32) -> Result<VecExpr, String> {
    if depth > MAX_TREE_DEPTH {
        return Err("native MIR: vector expression too deep".into());
    }
    let sub = |r: &mut Reader| read_vec_expr(r, depth + 1).map(Box::new);
    Ok(match r.u8()? {
        0 => VecExpr::Splat { v: r.v()?, ty: r.u8()? },
        1 => VecExpr::Index,
        2 => VecExpr::Load { array: r.v()?, off: r.v()?, ty: r.u8()? },
        3 => VecExpr::Bin { kind: r.u8()?, lhs: sub(r)?, rhs: sub(r)? },
        4 => VecExpr::Unary { kind: r.u8()?, src: sub(r)? },
        5 => VecExpr::Select { mask: sub(r)?, then: sub(r)?, els: sub(r)? },
        6 => VecExpr::Fma { ty: r.u8()?, a: sub(r)?, b: sub(r)?, c: sub(r)? },
        t => return Err(format!("native MIR: bad vector expression {t}")),
    })
}

fn read_vec_stores(r: &mut Reader) -> Result<Vec<(ValueId, ValueId, u8, VecExpr)>, String> {
    let n = r.len()?;
    (0..n).map(|_| Ok((r.v()?, r.v()?, r.u8()?, read_vec_expr(r, 0)?))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A body with a value in the fields the code tiers and the root
    /// analyses read, and a cleanup pad of its own name.
    fn body() -> NativeBody {
        let mut pad = MirFunc::new("pad of f");
        pad.homes = vec![MirTy::HeapRef];
        pad.home_base = Some(255);
        let mut f = MirFunc::new("f");
        f.blocks.clear();
        f.types = vec![MirTy::Value, MirTy::Bottom, MirTy::I64];
        f.params = vec![ValueId(0)];
        f.ret_ty = Some(MirTy::I64);
        f.ret_layout = MirLayout::TwoSlot;
        let loc = DebugLoc { file: 1, start_byte: 2, end_byte: 3, scope: 1 };
        let mut b = MirBlock::new(BlockId(0));
        b.insts.push(MirInst::Probe { dest: ValueId(1), id: 7 });
        b.insts.push(MirInst::GcBarrier { dest: ValueId(2), kind: MirGcKind::Safepoint, roots: vec![ValueId(0)] });
        f.blocks.push(b);
        let mut last = MirBlock::new(BlockId(1));
        last.term = Some(Terminator::Unreachable);
        f.blocks.push(last);
        f.value_locs.insert(ValueId(1), loc);
        f.op_locs.insert(ValueId(2), loc);
        f.term_locs.insert(BlockId(1), loc);
        f.slot_env.insert(ValueId(1), vec![(LocalId(3), ValueId(0))]);
        f.gc_roots.push(LiveRootSet { at: ValueId(1), values: vec![ValueId(0)], slots: vec![LocalId(3)] });
        f.debug_slots.insert(LocalId(3), ValueId(0));
        f.heap_slots.insert(LocalId(3));
        f.word_slots.insert(LocalId(4));
        f.local_binds.push((LocalId(3), ValueId(0)));
        f.emit_refusal = Some("existential pack");
        f.debug_keep = true;
        f.opaque_words = true;
        f.tail_targets.insert(Label(9));
        f.pair_twin = Some(Label(5));
        f.heap_pair_words.extend([ValueId(0), ValueId(2), ValueId(1)]);
        f.pure_calls.insert(ValueId(1));
        f.cleanup = Some(Box::new(pad));
        NativeBody { name: "f".into(), entry: Some(Label(7)), mir: f }
    }

    fn bytes(b: &NativeBody) -> Vec<u8> {
        encode(std::slice::from_ref(b), &HashMap::from([(b.name.clone(), 42)]), &scopes())
    }

    fn scopes() -> Vec<common::InlineScope> {
        vec![common::InlineScope { callee: "g".into(), call: DebugLoc { file: 1, start_byte: 8, end_byte: 9, scope: 0 }, at_call: false }]
    }

    #[test]
    fn the_inline_scopes_come_back() {
        let bytes = bytes(&body());
        assert_eq!(common::section_inline_scopes(&bytes), scopes());
        let (back, _) = decode(&bytes).expect("decode");
        assert_eq!(back[0].mir.op_loc(ValueId(2)).scope, 1);
    }

    /// A section of an archive before inline scopes (format 16) reads, with
    /// no scope table and no location in a scope.
    #[test]
    fn a_section_without_scopes_reads() {
        let b = body();
        let old = encode_as(std::slice::from_ref(&b), &HashMap::from([(b.name.clone(), 42)]), &scopes(), FORMAT_UNSCOPED);
        assert!(common::section_inline_scopes(&old).is_empty());
        let (back, pcs) = decode(&old).expect("decode format 16");
        assert_eq!(pcs.get("f"), Some(&42));
        assert_eq!(back[0].mir.op_loc(ValueId(2)), DebugLoc { scope: 0, ..b.mir.op_loc(ValueId(2)) });
    }

    #[test]
    fn a_full_body_comes_back_equal() {
        let b = body();
        let (back, pcs) = decode(&bytes(&b)).expect("decode");
        assert_eq!(pcs.get("f"), Some(&42));
        assert_eq!(back[0].entry, b.entry);
        assert_eq!(back[0].mir, b.mir);
    }

    #[test]
    fn the_same_mir_gives_the_same_bytes() {
        // Each `body()` has its own hash seeds, so a set written in hash
        // order would differ between them.
        let first = bytes(&body());
        for _ in 0..16 {
            assert_eq!(bytes(&body()), first);
        }
    }

    #[test]
    fn a_truncated_section_is_refused_at_every_length() {
        let section = bytes(&body());
        for end in 0..section.len() {
            assert!(decode(&section[..end]).is_err(), "at {end} of {}", section.len());
        }
    }
}
