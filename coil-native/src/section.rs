//! The native MIR section of a `.ccoil` (`ArchivedProgram::native_mir`).
//!
//! `coil compile --jit` stores the typed MIR of the bodies the native
//! backend compiles, so `coil run` can JIT an archive without the source.
//! Only those bodies are stored, and only the MIR the backend reads: the
//! encoder never meets an instruction the backend does not lower, and the
//! format grows with each tier.
//!
//! Layout (little-endian): the magic `CNMIR`, a format byte, then the
//! bodies. Each body is its name, bytecode entry PC (`u32::MAX`: none),
//! entry label, signature, value types and blocks. Labels are the
//! compile's own `Label` numbers; a call names its callee by label.

use std::collections::HashMap;
use common::DebugLoc;

use compiler::native_mir::{
    BlockId, Label, MirAllocKind, MirBinOp, MirBlock, MirCastKind, MirCmpOp, MirConst, MirFunc, MirGcKind, MirInst, MirTy,
    MirUnaryOp, NativeBody, Terminator, ValueId,
};

const MAGIC: &[u8; 5] = b"CNMIR";
/// Bump when the layout changes; a reader refuses any other format.
const FORMAT: u8 = 5;
const NO_PC: u32 = u32::MAX;

/// The section for the bodies of `bodies` the backend compiles, keyed to
/// their entry PCs. Empty when none compiles.
pub fn encode(bodies: &[NativeBody], entry_pcs: &HashMap<String, u32>) -> Vec<u8> {
    let plan = crate::plan(bodies, entry_pcs);
    if plan.compile.is_empty() {
        return Vec::new();
    }
    let mut w = Writer(Vec::new());
    w.0.extend_from_slice(MAGIC);
    w.u8(FORMAT);
    // The bodies that compile, then a stub of each body they call on the
    // VM (its name, PC, label and shape: what `plan` needs to call it).
    let mut stubs: Vec<usize> = plan.vm_calls.keys().copied().collect();
    stubs.sort_unstable();
    w.u32((plan.compile.len() + stubs.len()) as u32);
    for (&i, stub) in plan.compile.iter().map(|i| (i, false)).chain(stubs.iter().map(|i| (i, true))) {
        let body = &bodies[i];
        w.str(&body.name);
        w.u32(entry_pcs.get(&body.name).copied().unwrap_or(NO_PC));
        // `plan` keeps only bodies a call can name, so each has a label.
        w.u32(body.entry.map_or(u32::MAX, |l| l.0));
        if stub {
            w.u8(BODY_STUB);
            w.u32(body.mir.params.len() as u32);
            w.u8(u8::from(body.mir.ret_hi_ty.is_some()));
        } else {
            w.u8(BODY_FULL);
            func(&mut w, &body.mir);
        }
    }
    w.0
}

const BODY_FULL: u8 = 0;
const BODY_STUB: u8 = 1;

/// The bodies of a section and their entry PCs, ready for [`crate::jit`].
pub fn decode(bytes: &[u8]) -> Result<(Vec<NativeBody>, HashMap<String, u32>), String> {
    let mut r = Reader { b: bytes, i: 0 };
    if r.take(MAGIC.len())? != MAGIC {
        return Err("not a native MIR section".into());
    }
    let format = r.u8()?;
    if format != FORMAT {
        return Err(format!("native MIR format {format}, this coil reads {FORMAT}"));
    }
    let n = r.u32()?;
    let mut bodies = Vec::new();
    let mut pcs = HashMap::new();
    for _ in 0..n {
        let name = r.str()?;
        let pc = r.u32()?;
        let label = r.u32()?;
        let mir = match r.u8()? {
            BODY_FULL => read_func(&mut r, &name)?,
            BODY_STUB => {
                // A body that stays on the VM: native code calls it there.
                let mut f = MirFunc::new(&name);
                f.emit_refusal = Some("runs on the VM");
                f.params = (0..r.u32()?).map(ValueId).collect();
                if r.u8()? != 0 {
                    f.ret_hi_ty = Some(MirTy::I64);
                }
                f
            }
            k => return Err(format!("native MIR: bad body kind {k}")),
        };
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

const TYS: [MirTy; 6] = [MirTy::I64, MirTy::F64, MirTy::Bool, MirTy::HeapRef, MirTy::NicheOpt, MirTy::NicheRes];
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

/// Index of `x` in `table`. Types outside the numeric tier encode as
/// `0xff`, which [`Reader::pick`] refuses, so they never come back.
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

const T_JUMP: u8 = 0;
const T_BR: u8 = 1;
const T_RETURN: u8 = 2;
const T_UNREACHABLE: u8 = 3;
const T_MATCH: u8 = 4;
/// No result type ([`Writer::opt_ty`]), or a return without that word.
const NONE: u8 = 0xff;

struct Writer(Vec<u8>);

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
    w.u32(f.entry.0);
    w.u32(f.types.len() as u32);
    for &t in &f.types {
        // Unused value slots may hold any type; they stay `Bottom`.
        w.u8(if crate::numeric(t) { idx(&TYS, &t) } else { 0xfe });
    }
    w.vs(&f.params);
    w.opt_ty(f.ret_ty);
    w.opt_ty(f.ret_hi_ty);
    w.u32(f.blocks.len() as u32);
    for b in &f.blocks {
        w.u32(b.id.0);
        let insts: Vec<&MirInst> = b.insts.iter().filter(|i| !matches!(i, MirInst::Deopt { .. })).collect();
        w.u32(insts.len() as u32);
        for inst in insts {
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
            // `plan` admits no other terminator.
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
            _ => w.u8(T_UNREACHABLE),
        }
    }
    // Source locations of the ops a native panic names (see
    // `crate::Lower::site`): each instruction's own location, when known.
    let locs: Vec<(ValueId, DebugLoc)> = f
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .map(|i| (i.dest(), f.op_loc(i.dest())))
        .filter(|(_, l)| l.is_known())
        .collect();
    w.u32(locs.len() as u32);
    for (v, l) in locs {
        w.v(v);
        w.u32(l.file);
        w.u32(l.start_byte);
        w.u32(l.end_byte);
    }
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
        MirInst::ArrayLen { dest, array } => {
            w.u8(I_LEN);
            w.v(*dest);
            w.v(*array);
        }
        MirInst::HeapFieldLoad { dest, object, index } => {
            w.u8(I_FIELD);
            w.v(*dest);
            w.v(*object);
            w.u32(*index);
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
                MirAllocKind::Tuple => w.u8(1),
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
                MirAllocKind::PolyFn { target } => {
                    w.u8(5);
                    w.u32(target.0);
                }
                MirAllocKind::PolyFnCapture { target, params } => {
                    w.u8(6);
                    w.u32(target.0);
                    w.u32(params);
                }
                _ => unreachable!("native section: allocation outside the native tier"),
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
        MirInst::HostInvoke { dest, native_id, layout, args } => {
            w.u8(I_HOST);
            w.v(*dest);
            w.u32(u32::from(*native_id));
            w.u8(*layout);
            w.vs(args);
        }
        MirInst::CallIndirect { dest, callee, args, dicts } => {
            w.u8(I_CALL_INDIRECT);
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
        MirInst::GcBarrier { dest, .. } => {
            // Native code only needs the pairing, which order gives.
            w.u8(I_GC_BARRIER);
            w.v(*dest);
        }
        // `plan` admits nothing else, and `func` drops `Deopt`.
        _ => unreachable!("native section: instruction outside the numeric tier"),
    }
}

// ---------------------------------------------------------------------------
// Decoding

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
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
    fn pick<T: Copy>(&mut self, table: &[T], what: &str) -> Result<T, String> {
        let i = self.u8()?;
        table.get(i as usize).copied().ok_or_else(|| format!("native MIR: bad {what} {i}"))
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

fn read_func(r: &mut Reader, name: &str) -> Result<MirFunc, String> {
    let mut f = MirFunc::new(name);
    f.blocks.clear();
    f.entry = BlockId(r.u32()?);
    let n = r.len()?;
    f.types = (0..n)
        .map(|_| match r.u8()? {
            0xfe => Ok(MirTy::Bottom),
            i => TYS.get(i as usize).copied().ok_or_else(|| format!("native MIR: bad type {i}")),
        })
        .collect::<Result<_, String>>()?;
    f.params = r.vs()?;
    f.ret_ty = r.opt_ty()?;
    f.ret_hi_ty = r.opt_ty()?;
    let nb = r.len()?;
    for _ in 0..nb {
        let mut b = MirBlock::new(BlockId(r.u32()?));
        let ni = r.len()?;
        for _ in 0..ni {
            b.insts.push(read_inst(r)?);
        }
        b.term = Some(match r.u8()? {
            T_JUMP => Terminator::Jump { dest: BlockId(r.u32()?) },
            T_BR => Terminator::Br { cond: r.v()?, taken: BlockId(r.u32()?), not_taken: BlockId(r.u32()?) },
            T_RETURN => Terminator::Return { lo: r.opt_v()?, hi: r.opt_v()? },
            T_UNREACHABLE => Terminator::Unreachable,
            T_MATCH => {
                let (scrutinee, tag) = (r.v()?, r.u32()?);
                let n = r.len()?;
                let payloads = (0..n).map(|_| r.v()).collect::<Result<Vec<_>, _>>()?;
                Terminator::JumpIfMatch { scrutinee, tag, payloads, taken: BlockId(r.u32()?), not_taken: BlockId(r.u32()?) }
            }
            t => return Err(format!("native MIR: bad terminator {t}")),
        });
        f.blocks.push(b);
    }
    let nl = r.len()?;
    for _ in 0..nl {
        let v = r.v()?;
        let loc = DebugLoc { file: r.u32()?, start_byte: r.u32()?, end_byte: r.u32()? };
        f.op_locs.insert(v, loc);
    }
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
        I_FIELD => MirInst::HeapFieldLoad { dest: r.v()?, object: r.v()?, index: r.u32()? },
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
                1 => MirAllocKind::Tuple,
                2 => MirAllocKind::Enum { tag: r.u32()? },
                3 => MirAllocKind::Object { type_id: r.u32()?, nfields: r.u32()? },
                4 => MirAllocKind::Fn { target: Label(r.u32()?), operand: r.u32()?, mask: r.u32()? },
                5 => MirAllocKind::PolyFn { target: Label(r.u32()?) },
                6 => MirAllocKind::PolyFnCapture { target: Label(r.u32()?), params: r.u32()? },
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
        I_CALL_INDIRECT => MirInst::CallIndirect { dest: r.v()?, callee: r.v()?, args: r.vs()?, dicts: r.u32()? },
        I_HOST => MirInst::HostInvoke {
            dest: r.v()?,
            native_id: u16::try_from(r.u32()?).map_err(|_| "native MIR: bad host id")?,
            layout: r.u8()?,
            args: r.vs()?,
        },
        I_FREE_EARLY => MirInst::FreeEarly { dest: r.v()?, value: r.v()?, deep: r.u8()? != 0 },
        I_FORMAT => MirInst::Format { dest: r.v()?, fmt: r.v()?, args: r.vs()? },
        I_STRINGIFY => MirInst::Stringify { dest: r.v()?, src: r.v()?, tag: Some(r.u32()?).filter(|&t| t != u32::MAX) },
        I_STR_EQ => MirInst::StrEq { dest: r.v()?, lhs: r.v()?, rhs: r.v()?, ne: r.u8()? != 0 },
        I_CLEAR_WORD => MirInst::ClearWord { dest: r.v()?, value: r.v()? },
        I_MATCH_PAYLOAD => MirInst::MatchPayload { dest: r.v()?, scrutinee: r.v()?, index: r.u32()? },
        I_ARRAY_PUSH => MirInst::ArrayPush { dest: r.v()?, array: r.v()?, value: r.v()? },
        I_GC_BARRIER => MirInst::GcBarrier { dest: r.v()?, kind: MirGcKind::Safepoint, roots: Vec::new() },
        I_COW => {
            let (dest, src, unique) = (r.v()?, r.v()?, r.u8()? != 0);
            let field = Some(r.u32()?).filter(|&f| f != u32::MAX);
            MirInst::Cow { dest, src, unique, field }
        }
        t => return Err(format!("native MIR: bad instruction {t}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use machine::native::{NATIVE_STACK_BUDGET, NativeCtx};

    /// `fn f(int a, int b) -> int { return a + b * 2; }`
    fn body() -> NativeBody {
        let mut f = MirFunc::new("f");
        f.blocks.clear();
        f.types = vec![MirTy::I64; 5];
        f.params = vec![ValueId(0), ValueId(1)];
        f.ret_ty = Some(MirTy::I64);
        let mut b = MirBlock::new(BlockId(0));
        b.insts.push(MirInst::Const { dest: ValueId(2), c: MirConst::I64(2) });
        b.insts.push(MirInst::Bin { dest: ValueId(3), op: MirBinOp::Mul, ty: MirTy::I64, lhs: ValueId(1), rhs: ValueId(2) });
        b.insts.push(MirInst::Bin { dest: ValueId(4), op: MirBinOp::Add, ty: MirTy::I64, lhs: ValueId(0), rhs: ValueId(3) });
        b.term = Some(Terminator::Return { lo: Some(ValueId(4)), hi: None });
        f.blocks.push(b);
        NativeBody { name: "f".into(), entry: Some(Label(7)), mir: f }
    }

    #[test]
    fn section_round_trips_and_runs() {
        let pcs = HashMap::from([("f".to_string(), 42u32)]);
        let bytes = encode(&[body()], &pcs);
        let (bodies, back_pcs) = decode(&bytes).expect("decode");
        assert_eq!(back_pcs, pcs);
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].entry, Some(Label(7)));
        assert_eq!(bodies[0].mir.blocks, body().mir.blocks);
        assert_eq!(bodies[0].mir.params, body().mir.params);

        let program = crate::jit(&bodies, &back_pcs).expect("jit");
        assert_eq!(program.entries.len(), 1);
        assert_eq!(program.entries[0].pc, 42);
        let mut undo = Vec::new();
        let mut ctx = NativeCtx::new(0, &mut undo);
        let marker = 0u8;
        ctx.stack_limit = (std::ptr::addr_of!(marker) as u64).saturating_sub(NATIVE_STACK_BUDGET);
        let args = [5u64, 10];
        // SAFETY: compiled for two argument words.
        let out = unsafe { (program.entries[0].entry)(&mut ctx, args.as_ptr()) };
        assert_eq!((ctx.bail, out), (0, 25));
    }

    #[test]
    fn nothing_to_compile_is_an_empty_section() {
        let mut b = body();
        b.mir.types[4] = MirTy::HeapRef;
        assert!(encode(&[b], &HashMap::new()).is_empty());
    }

    #[test]
    fn corrupt_sections_are_refused() {
        let bytes = encode(&[body()], &HashMap::new());
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
        assert!(decode(b"CNMIR\x02").is_err());
        assert!(decode(b"nope").is_err());
        let mut long = bytes.clone();
        long.push(0);
        assert!(decode(&long).is_err());
    }
}
