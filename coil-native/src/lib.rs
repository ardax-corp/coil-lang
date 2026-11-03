//! Native code for Coil: typed MIR to machine code with Cranelift.
//!
//! Step 2 of `feature/native` (see `docs/internals/native.md`): the
//! *numeric tier*. A body compiles when every value it touches is an
//! `int`, `float` or `bool` and every call it makes goes to another body
//! that compiles. Such a function is pure, so native code never has to
//! reproduce a VM panic: on an int trap, an operation it does not model or
//! a recursion past its stack budget it sets the bail flag and returns, and
//! the VM runs the call again in bytecode (see `machine::native`).
//!
//! [`jit`] compiles in memory for `coil --jit`. An object-file driver for
//! `coil package` reuses the same lowering later.

pub mod section;

use std::collections::{HashMap, HashSet};

use compiler::native_mir::{
    BlockId, Label, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirFunc, MirInst, MirTy, MirUnaryOp,
    NativeBody, Terminator, ValueId,
};
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{AbiParam, Block, FuncRef, InstBuilder, MemFlagsData, Signature, Type, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use machine::native::{
    NATIVE_CTX_BAIL, NATIVE_CTX_STACK_LIMIT, NATIVE_CTX_UNDO_BUF, NATIVE_CTX_UNDO_CAP, NATIVE_CTX_UNDO_LEN,
    NativeEntry, NativeEntryFn,
};

/// What [`jit`] produced.
pub struct NativeProgram {
    /// Entries to install with `Machine::install_native`.
    pub entries: Vec<NativeEntry>,
    /// Bodies compiled to native code.
    pub compiled: Vec<String>,
    /// Bodies left to the VM, with the reason.
    pub refused: Vec<(String, String)>,
}

/// Compile every eligible body of `bodies` in memory. `entry_pcs` maps a
/// body's name to its bytecode entry PC. The code stays mapped for the
/// rest of the process.
pub fn jit(bodies: &[NativeBody], entry_pcs: &HashMap<String, u32>) -> Result<NativeProgram, String> {
    // A body `plan` admits may still fail to lower; its callers already
    // name it, so leave it out and build the module again.
    let mut excluded = HashMap::new();
    loop {
        match jit_once(bodies, entry_pcs, &excluded)? {
            Attempt::Done(program) => return Ok(program),
            Attempt::Without(i, why) => {
                excluded.insert(i, why);
            }
        }
    }
}

enum Attempt {
    Done(NativeProgram),
    /// Body `i` does not lower: try again without it.
    Without(usize, String),
}

fn jit_once(
    bodies: &[NativeBody],
    entry_pcs: &HashMap<String, u32>,
    excluded: &HashMap<usize, String>,
) -> Result<Attempt, String> {
    let plan = plan_without(bodies, excluded);
    let refused = plan.refused;
    if plan.compile.is_empty() {
        return Ok(Attempt::Done(NativeProgram { entries: Vec::new(), compiled: Vec::new(), refused }));
    }

    let mut flags = settings::builder();
    flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
    let isa_builder = cranelift_native::builder().map_err(|e| e.to_string())?;
    let isa = isa_builder
        .finish(settings::Flags::new(flags))
        .map_err(|e| e.to_string())?;
    let mut jb = JITBuilder::with_isa(isa, default_libcall_names());
    jb.symbol("coil_native_fmod", coil_native_fmod as *const u8);
    jb.symbol("coil_native_powf", coil_native_powf as *const u8);
    jb.symbol("coil_native_index", machine::native::coil_native_index as *const u8);
    jb.symbol("coil_native_len", machine::native::coil_native_len as *const u8);
    jb.symbol("coil_native_field", machine::native::coil_native_field as *const u8);
    jb.symbol("coil_native_store_index", machine::native::coil_native_store_index as *const u8);
    jb.symbol("coil_native_set_field", machine::native::coil_native_set_field as *const u8);
    jb.symbol("coil_native_view", machine::native::coil_native_view as *const u8);
    jb.symbol("coil_native_undo_grow", machine::native::coil_native_undo_grow as *const u8);
    jb.symbol("coil_native_cow", machine::native::coil_native_cow as *const u8);
    jb.symbol("coil_native_cow_field", machine::native::coil_native_cow_field as *const u8);
    jb.symbol("coil_native_alloc", machine::native::coil_native_alloc as *const u8);
    jb.symbol("coil_native_array_push", machine::native::coil_native_array_push as *const u8);
    jb.symbol("coil_native_enum_tag", machine::native::coil_native_enum_tag as *const u8);
    jb.symbol("coil_native_enum_payload", machine::native::coil_native_enum_payload as *const u8);
    let mut module = JITModule::new(jb);

    let ids = declare(&mut module, bodies, &plan.compile)?;
    let fcfg = module.isa().frontend_config();
    let mut fbc = FunctionBuilderContext::new();
    let mut ctx = module.make_context();
    let mut defined = Vec::new();
    for &i in &plan.compile {
        let body = &bodies[i];
        ctx.func.signature = impl_signature(&module, &body.mir);
        let lowered = {
            let mut lower = Lower::new(&mut module, &mut ctx.func, &mut fbc, &ids, &plan.by_label, bodies);
            let r = lower.body(&body.mir);
            if r.is_ok() {
                lower.b.finalize(fcfg);
            }
            r
        };
        if let Err(why) = lowered {
            return Ok(Attempt::Without(i, why));
        }
        if let Err(e) = module.define_function(ids.imp[&i], &mut ctx) {
            return Ok(Attempt::Without(i, format!("cranelift: {e:?}")));
        }
        module.clear_context(&mut ctx);

        ctx.func.signature = entry_signature(&module);
        build_entry(&mut module, &mut ctx.func, &mut fbc, ids.imp[&i], &body.mir);
        module
            .define_function(ids.entry[&i], &mut ctx)
            .map_err(|e| format!("cranelift entry of `{}`: {e:?}", body.name))?;
        module.clear_context(&mut ctx);
        defined.push(i);
    }
    module.finalize_definitions().map_err(|e| e.to_string())?;

    let mut entries = Vec::new();
    let mut compiled = Vec::new();
    for i in defined {
        let body = &bodies[i];
        compiled.push(body.name.clone());
        let Some(&pc) = entry_pcs.get(&body.name) else {
            continue;
        };
        let ptr = module.get_finalized_function(ids.entry[&i]);
        // SAFETY: `build_entry` gave this function the `NativeEntryFn` ABI.
        let entry: NativeEntryFn = unsafe { std::mem::transmute::<*const u8, NativeEntryFn>(ptr) };
        entries.push(NativeEntry { pc, arity: body.mir.params.len() as u16, entry });
    }
    // The code must outlive every `Machine` that runs it.
    std::mem::forget(module);
    Ok(Attempt::Done(NativeProgram { entries, compiled, refused }))
}

extern "C" fn coil_native_fmod(a: f64, b: f64) -> f64 {
    a % b
}

extern "C" fn coil_native_powf(a: f64, b: f64) -> f64 {
    a.powf(b)
}

// ---------------------------------------------------------------------------
// Which bodies compile

struct Plan {
    /// Indices into `bodies` that compile.
    compile: Vec<usize>,
    by_label: HashMap<Label, usize>,
    refused: Vec<(String, String)>,
}

fn plan(bodies: &[NativeBody]) -> Plan {
    plan_without(bodies, &HashMap::new())
}

/// [`plan`] with `excluded` bodies refused for the given reason.
fn plan_without(bodies: &[NativeBody], excluded: &HashMap<usize, String>) -> Plan {
    let by_label: HashMap<Label, usize> = bodies
        .iter()
        .enumerate()
        .filter_map(|(i, b)| Some((b.entry?, i)))
        .collect();
    let mut refused = Vec::new();
    let mut ok: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, b) in bodies.iter().enumerate() {
        if let Some(why) = excluded.get(&i) {
            refused.push((b.name.clone(), why.clone()));
            continue;
        }
        match eligible(&b.mir, &by_label) {
            Ok(callees) => {
                ok.insert(i, callees);
            }
            Err(why) => refused.push((b.name.clone(), why)),
        }
    }
    // Drop bodies that call a body that does not compile, to a fixpoint.
    loop {
        let bad: Vec<usize> = ok
            .iter()
            .filter(|(_, callees)| callees.iter().any(|c| !ok.contains_key(c)))
            .map(|(&i, _)| i)
            .collect();
        if bad.is_empty() {
            break;
        }
        for i in bad {
            ok.remove(&i);
            refused.push((bodies[i].name.clone(), "calls a body that stays on the VM".to_string()));
        }
    }
    let mut compile: Vec<usize> = ok.into_keys().collect();
    compile.sort_unstable();
    Plan { compile, by_label, refused }
}

/// A value native code holds: a scalar, or a heap word it passes along and
/// reads through (heap tier).
fn numeric(t: MirTy) -> bool {
    scalar(t) || heap_word(t)
}

/// A value native arithmetic works on.
fn scalar(t: MirTy) -> bool {
    matches!(t, MirTy::I64 | MirTy::F64 | MirTy::Bool)
}

/// A heap object word (a pointer, or a niche `Option` / `Result` word).
fn heap_word(t: MirTy) -> bool {
    matches!(t, MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes)
}

fn ty_of(f: &MirFunc, v: ValueId) -> MirTy {
    f.types.get(v.index()).copied().unwrap_or(MirTy::Bottom)
}

/// A word's kind for an object's packed word kinds, as the VM's emission
/// records them.
fn word_kind(t: MirTy) -> u8 {
    match t {
        MirTy::I64 | MirTy::F64 | MirTy::Bool => common::WORD_SCALAR,
        MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes => common::WORD_POINTER,
        MirTy::Value | MirTy::Bottom => common::WORD_UNKNOWN,
    }
}

/// Each `GcBarrier`'s object: the dest of the allocating instruction right
/// before it in its block, as the VM's emission pairs them. Native code
/// never collects during a call, so the barrier only names that word.
fn barrier_pairs(f: &MirFunc) -> HashMap<ValueId, ValueId> {
    let mut out = HashMap::new();
    for b in &f.blocks {
        let mut pending = None;
        for inst in &b.insts {
            match inst {
                MirInst::Alloc { dest, .. }
                | MirInst::ArrayPush { dest, .. }
                | MirInst::Format { dest, .. }
                | MirInst::Stringify { dest, .. }
                | MirInst::Cow { dest, .. } => pending = Some(*dest),
                MirInst::GcBarrier { dest, .. } => {
                    if let Some(obj) = pending.take() {
                        out.entry(*dest).or_insert(obj);
                    }
                }
                _ => pending = None,
            }
        }
    }
    out
}

/// The callees of `f` when its body is in the numeric tier.
fn eligible(f: &MirFunc, by_label: &HashMap<Label, usize>) -> Result<Vec<usize>, String> {
    if let Some(why) = f.emit_refusal {
        return Err(format!("no emission: {why}"));
    }
    if f.opaque_words {
        return Err("opaque words".into());
    }
    if f.cleanup.is_some() {
        return Err("defer".into());
    }
    if f.params.len() > 16 {
        return Err("more than 16 parameters".into());
    }
    for &p in &f.params {
        if !numeric(ty_of(f, p)) {
            return Err(format!("parameter type {:?}", ty_of(f, p)));
        }
    }
    match (f.ret_ty, f.ret_hi_ty) {
        (Some(t), None) if numeric(t) => {}
        other => return Err(format!("return type {other:?}")),
    }
    let mut callees = Vec::new();
    let pairs = barrier_pairs(f);
    let need = |v: &ValueId| -> Result<(), String> {
        let t = ty_of(f, *v);
        if numeric(t) { Ok(()) } else { Err(format!("value type {t:?}")) }
    };
    let need_scalar = |v: &ValueId| -> Result<(), String> {
        let t = ty_of(f, *v);
        if scalar(t) { Ok(()) } else { Err(format!("{t:?} in arithmetic")) }
    };
    let need_heap = |v: &ValueId| -> Result<(), String> {
        let t = ty_of(f, *v);
        if t == MirTy::HeapRef { Ok(()) } else { Err(format!("{t:?} read as an object")) }
    };
    for b in &f.blocks {
        for inst in &b.insts {
            let mut one = || -> Result<(), String> {
                match inst {
                    MirInst::Const { dest, .. } => need(dest)?,
                    // Bit ops are exact on any word: niche matches tag and
                    // untag `Option` / `Result` words with them.
                    MirInst::Bin { dest, op: MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor, ty, lhs, rhs }
                        if *ty != MirTy::F64 =>
                    {
                        need(dest)?;
                        need(lhs)?;
                        need(rhs)?;
                    }
                    MirInst::Bin { dest, op, ty, lhs, rhs } => {
                        need_scalar(dest)?;
                        need_scalar(lhs)?;
                        need_scalar(rhs)?;
                        if *ty == MirTy::I64 && *op == MirBinOp::Pow {
                            return Err("int **".into());
                        }
                        if *ty == MirTy::Bool && !op.requires_int() {
                            return Err("bool arithmetic".into());
                        }
                    }
                    MirInst::Cmp { dest, ty, lhs, rhs, .. } => {
                        if !scalar(*ty) {
                            return Err(format!("compare of {ty:?}"));
                        }
                        need_scalar(dest)?;
                        need_scalar(lhs)?;
                        need_scalar(rhs)?;
                    }
                    // `!w` is `w == 0`, exact on any word (`None` is 0).
                    MirInst::Unary { dest, op: MirUnaryOp::Not, src } if ty_of(f, *src) != MirTy::F64 => {
                        need_scalar(dest)?;
                        need(src)?;
                    }
                    MirInst::Unary { dest, src, .. } => {
                        need_scalar(dest)?;
                        need_scalar(src)?;
                    }
                    MirInst::Cast { dest, kind, src, .. } => {
                        if *kind == MirCastKind::Word {
                            need(dest)?;
                            need(src)?;
                        } else {
                            need_scalar(dest)?;
                            need_scalar(src)?;
                        }
                    }
                    MirInst::Index { dest, array, index, .. } => {
                        need(dest)?;
                        need_heap(array)?;
                        need_scalar(index)?;
                    }
                    MirInst::ArrayLen { dest, array } => {
                        need_scalar(dest)?;
                        need_heap(array)?;
                    }
                    MirInst::HeapFieldLoad { dest, object, .. } => {
                        need(dest)?;
                        need_heap(object)?;
                    }
                    MirInst::StoreIndex { dest, array, index, value, .. } => {
                        need(dest)?;
                        need_heap(array)?;
                        need_scalar(index)?;
                        need(value)?;
                    }
                    MirInst::HeapFieldStore { dest, object, value, .. } => {
                        need(dest)?;
                        need_heap(object)?;
                        need(value)?;
                    }
                    MirInst::Alloc { dest, kind, elems, .. } => {
                        need(dest)?;
                        for e in elems {
                            need(e)?;
                        }
                        match kind {
                            MirAllocKind::Array | MirAllocKind::Tuple | MirAllocKind::Enum { .. } => {}
                            MirAllocKind::Object { .. } if elems.is_empty() => {}
                            other => return Err(format!("allocation {other:?}")),
                        }
                    }
                    MirInst::ArrayPush { dest, array, value } => {
                        need(dest)?;
                        need_heap(array)?;
                        need(value)?;
                    }
                    MirInst::GcBarrier { dest, .. } => {
                        need(dest)?;
                        if !pairs.contains_key(dest) {
                            return Err("GcBarrier without an allocation".into());
                        }
                    }
                    MirInst::Cow { dest, src, field, .. } => {
                        need(dest)?;
                        if field.is_some() {
                            need_heap(src)?;
                        } else {
                            need(src)?;
                        }
                    }
                    MirInst::Phi { dest, args, .. } => {
                        need(dest)?;
                        for (_, v) in args {
                            need(v)?;
                        }
                    }
                    MirInst::Call { dest, dest_hi, target, args } => {
                        if dest_hi.is_some() {
                            return Err("two-word call".into());
                        }
                        need(dest)?;
                        for a in args {
                            need(a)?;
                        }
                        let Some(&callee) = by_label.get(target) else {
                            return Err("call to an unknown body".into());
                        };
                        callees.push(callee);
                    }
                    MirInst::MatchPayload { dest, scrutinee, .. } => {
                        need(dest)?;
                        if !heap_word(ty_of(f, *scrutinee)) {
                            return Err(format!("match payload of {:?}", ty_of(f, *scrutinee)));
                        }
                    }
                    MirInst::Deopt { .. } => {}
                    other => return Err(format!("instruction {}", inst_name(other))),
                }
                Ok(())
            };
            one().map_err(|e| if e.starts_with("instruction ") { e } else { format!("{e} in {}", inst_name(inst)) })?;
        }
        match &b.term {
            Some(Terminator::Jump { .. }) | Some(Terminator::Unreachable) => {}
            Some(Terminator::Br { cond, .. }) => need_scalar(cond)?,
            Some(Terminator::Return { lo: Some(v), hi: None }) => need(v)?,
            Some(Terminator::JumpIfMatch { scrutinee, tag, payloads, .. }) => {
                match ty_of(f, *scrutinee) {
                    MirTy::HeapRef => {}
                    // A niche word is its own payload, read with `MatchPayload`.
                    MirTy::NicheOpt | MirTy::NicheRes if *tag <= 1 && payloads.is_empty() => {}
                    t => return Err(format!("match on {t:?} tag {tag}")),
                }
                for p in payloads {
                    need(p)?;
                }
            }
            Some(t) => return Err(format!("terminator {t:?}").chars().take(60).collect()),
            None => return Err("block without terminator".into()),
        }
    }
    Ok(callees)
}

fn inst_name(i: &MirInst) -> String {
    let s = format!("{i:?}");
    s.split([' ', '{', '(']).next().unwrap_or("?").to_string()
}

// ---------------------------------------------------------------------------
// Declarations

struct Ids {
    /// Native-convention body: `(ctx, params..) -> ret`.
    imp: HashMap<usize, FuncId>,
    /// VM entry: `(ctx, *const u64) -> u64`.
    entry: HashMap<usize, FuncId>,
    fmod: FuncId,
    powf: FuncId,
    /// Heap reads (`machine::native::coil_native_*`).
    index: FuncId,
    len: FuncId,
    field: FuncId,
    store_index: FuncId,
    set_field: FuncId,
    view: FuncId,
    undo_grow: FuncId,
    /// Copy-on-write (these allocate, never collect).
    cow: FuncId,
    cow_field: FuncId,
    /// Allocation (`coil_native_alloc`, `coil_native_array_push`).
    alloc: FuncId,
    array_push: FuncId,
    /// Boxed enum matches.
    enum_tag: FuncId,
    enum_payload: FuncId,
}

fn cl_ty(t: MirTy) -> Type {
    if t == MirTy::F64 { types::F64 } else { types::I64 }
}

fn impl_signature(module: &JITModule, f: &MirFunc) -> Signature {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    for &p in &f.params {
        sig.params.push(AbiParam::new(cl_ty(ty_of(f, p))));
    }
    sig.returns.push(AbiParam::new(cl_ty(f.ret_ty.unwrap_or(MirTy::I64))));
    sig
}

fn entry_signature(module: &JITModule) -> Signature {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    sig
}

fn declare(module: &mut JITModule, bodies: &[NativeBody], compile: &[usize]) -> Result<Ids, String> {
    let e = |e: cranelift_module::ModuleError| e.to_string();
    let mut imp = HashMap::new();
    let mut entry = HashMap::new();
    for &i in compile {
        let sig = impl_signature(module, &bodies[i].mir);
        imp.insert(i, module.declare_function(&format!("coil_impl_{i}"), Linkage::Local, &sig).map_err(e)?);
        let esig = entry_signature(module);
        entry.insert(i, module.declare_function(&format!("coil_entry_{i}"), Linkage::Local, &esig).map_err(e)?);
    }
    let mut f2 = module.make_signature();
    f2.params.push(AbiParam::new(types::F64));
    f2.params.push(AbiParam::new(types::F64));
    f2.returns.push(AbiParam::new(types::F64));
    let fmod = module.declare_function("coil_native_fmod", Linkage::Import, &f2).map_err(e)?;
    let powf = module.declare_function("coil_native_powf", Linkage::Import, &f2).map_err(e)?;
    let words = |n: usize| {
        let mut sig = module.make_signature();
        sig.params.extend((0..n).map(|_| AbiParam::new(types::I64)));
        sig.returns.push(AbiParam::new(types::I64));
        sig
    };
    let (s5, s4, s3, s1, s2) = (words(5), words(4), words(3), words(1), words(2));
    let index = module.declare_function("coil_native_index", Linkage::Import, &s3).map_err(e)?;
    let len = module.declare_function("coil_native_len", Linkage::Import, &s1).map_err(e)?;
    let field = module.declare_function("coil_native_field", Linkage::Import, &s2).map_err(e)?;
    let store_index = module.declare_function("coil_native_store_index", Linkage::Import, &s4).map_err(e)?;
    let set_field = module.declare_function("coil_native_set_field", Linkage::Import, &s4).map_err(e)?;
    let view = module.declare_function("coil_native_view", Linkage::Import, &s3).map_err(e)?;
    let mut s1v = module.make_signature();
    s1v.params.push(AbiParam::new(types::I64));
    let undo_grow = module.declare_function("coil_native_undo_grow", Linkage::Import, &s1v).map_err(e)?;
    let cow = module.declare_function("coil_native_cow", Linkage::Import, &s3).map_err(e)?;
    let cow_field = module.declare_function("coil_native_cow_field", Linkage::Import, &s3).map_err(e)?;
    let alloc = module.declare_function("coil_native_alloc", Linkage::Import, &s5).map_err(e)?;
    let array_push = module.declare_function("coil_native_array_push", Linkage::Import, &s3).map_err(e)?;
    let enum_tag = module.declare_function("coil_native_enum_tag", Linkage::Import, &s1).map_err(e)?;
    let enum_payload = module.declare_function("coil_native_enum_payload", Linkage::Import, &s3).map_err(e)?;
    Ok(Ids {
        imp,
        entry,
        fmod,
        powf,
        index,
        len,
        field,
        store_index,
        set_field,
        view,
        undo_grow,
        cow,
        cow_field,
        alloc,
        array_push,
        enum_tag,
        enum_payload,
    })
}

// ---------------------------------------------------------------------------
// The VM entry wrapper

fn build_entry(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
    fbc: &mut FunctionBuilderContext,
    imp: FuncId,
    f: &MirFunc,
) {
    let callee = module.declare_func_in_func(imp, func);
    let mut b = FunctionBuilder::new(func, fbc);
    let blk = b.create_block();
    b.append_block_params_for_function_params(blk);
    b.switch_to_block(blk);
    let ctx = b.block_params(blk)[0];
    let argp = b.block_params(blk)[1];
    let mut args = vec![ctx];
    for (i, &p) in f.params.iter().enumerate() {
        let w = b.ins().load(types::I64, MemFlagsData::trusted(), argp, (i * 8) as i32);
        let v = match ty_of(f, p) {
            MirTy::F64 => b.ins().bitcast(types::F64, MemFlagsData::new(), w),
            // The VM reads a bool word's low byte: `== 1` is true.
            MirTy::Bool => {
                let low = b.ins().band_imm_u(w, 0xff);
                let t = b.ins().icmp_imm_s(IntCC::Equal, low, 1);
                b.ins().uextend(types::I64, t)
            }
            _ => w,
        };
        args.push(v);
    }
    let call = b.ins().call(callee, &args);
    let r = b.inst_results(call)[0];
    let out = if f.ret_ty == Some(MirTy::F64) {
        b.ins().bitcast(types::I64, MemFlagsData::new(), r)
    } else {
        r
    };
    b.ins().return_(&[out]);
    b.seal_all_blocks();
    b.finalize(module.isa().frontend_config());
}

// ---------------------------------------------------------------------------
// Body lowering

struct Lower<'a> {
    b: FunctionBuilder<'a>,
    module_refs: HashMap<usize, FuncRef>,
    fmod: FuncRef,
    powf: FuncRef,
    index: FuncRef,
    len: FuncRef,
    field: FuncRef,
    store_index: FuncRef,
    set_field: FuncRef,
    view: FuncRef,
    undo_grow: FuncRef,
    cow: FuncRef,
    cow_field: FuncRef,
    alloc: FuncRef,
    array_push: FuncRef,
    enum_tag: FuncRef,
    enum_payload: FuncRef,
    /// Arrays this body indexes: `(ok, pointer, length)` of their element
    /// words in place, set where the array value is defined (see
    /// `machine::native::coil_native_view`).
    views: HashMap<ValueId, (Variable, Variable, Variable)>,
    /// Array values this body stores into.
    stored: HashSet<ValueId>,
    /// `GcBarrier` dest -> the object it names ([`barrier_pairs`]).
    barriers: HashMap<ValueId, ValueId>,
    /// Two words `coil_native_view` writes to.
    view_slot: cranelift_codegen::ir::StackSlot,
    by_label: &'a HashMap<Label, usize>,
    vars: Vec<Option<Variable>>,
    blocks: HashMap<BlockId, Block>,
    ctx: cranelift_codegen::ir::Value,
    bail: Block,
    ret: Type,
}

impl<'a> Lower<'a> {
    fn new(
        module: &mut JITModule,
        func: &'a mut cranelift_codegen::ir::Function,
        fbc: &'a mut FunctionBuilderContext,
        ids: &Ids,
        by_label: &'a HashMap<Label, usize>,
        _bodies: &[NativeBody],
    ) -> Self {
        let mut module_refs = HashMap::new();
        for (&i, &id) in &ids.imp {
            module_refs.insert(i, module.declare_func_in_func(id, func));
        }
        let fmod = module.declare_func_in_func(ids.fmod, func);
        let powf = module.declare_func_in_func(ids.powf, func);
        let index = module.declare_func_in_func(ids.index, func);
        let len = module.declare_func_in_func(ids.len, func);
        let field = module.declare_func_in_func(ids.field, func);
        let store_index = module.declare_func_in_func(ids.store_index, func);
        let set_field = module.declare_func_in_func(ids.set_field, func);
        let view = module.declare_func_in_func(ids.view, func);
        let undo_grow = module.declare_func_in_func(ids.undo_grow, func);
        let cow = module.declare_func_in_func(ids.cow, func);
        let cow_field = module.declare_func_in_func(ids.cow_field, func);
        let alloc = module.declare_func_in_func(ids.alloc, func);
        let array_push = module.declare_func_in_func(ids.array_push, func);
        let enum_tag = module.declare_func_in_func(ids.enum_tag, func);
        let enum_payload = module.declare_func_in_func(ids.enum_payload, func);
        let view_slot = func.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
            16,
            3,
        ));
        let ret = func.signature.returns[0].value_type;
        let mut b = FunctionBuilder::new(func, fbc);
        let bail = b.create_block();
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        let ctx = b.block_params(entry)[0];
        b.switch_to_block(entry);
        Self {
            b,
            module_refs,
            fmod,
            powf,
            index,
            len,
            field,
            store_index,
            set_field,
            view,
            undo_grow,
            cow,
            cow_field,
            alloc,
            array_push,
            enum_tag,
            enum_payload,
            views: HashMap::new(),
            stored: HashSet::new(),
            barriers: HashMap::new(),
            view_slot,
            by_label,
            vars: Vec::new(),
            blocks: HashMap::new(),
            ctx,
            bail,
            ret,
        }
    }

    fn var(&mut self, f: &MirFunc, v: ValueId) -> Variable {
        if self.vars.len() <= v.index() {
            self.vars.resize(v.index() + 1, None);
        }
        if let Some(var) = self.vars[v.index()] {
            return var;
        }
        let var = self.b.declare_var(cl_ty(ty_of(f, v)));
        self.vars[v.index()] = Some(var);
        var
    }

    fn get(&mut self, f: &MirFunc, v: ValueId) -> cranelift_codegen::ir::Value {
        let var = self.var(f, v);
        self.b.use_var(var)
    }

    fn set(&mut self, f: &MirFunc, v: ValueId, x: cranelift_codegen::ir::Value) {
        let var = self.var(f, v);
        let x = self.coerce(x, cl_ty(ty_of(f, v)));
        self.b.def_var(var, x);
    }

    /// `x` as a `want` word with the same bits. MIR may move a word between
    /// an `f64` and an `i64` value unchanged (a callee's `float` result into
    /// a caller's `int`-typed value, as the VM's untyped words allow).
    fn coerce(&mut self, x: cranelift_codegen::ir::Value, want: Type) -> cranelift_codegen::ir::Value {
        if self.b.func.dfg.value_type(x) == want {
            x
        } else {
            self.b.ins().bitcast(want, MemFlagsData::new(), x)
        }
    }

    /// Branch to the bail block when `cond` is non-zero; continue in a new block.
    fn bail_if(&mut self, cond: cranelift_codegen::ir::Value) {
        let next = self.b.create_block();
        self.b.ins().brif(cond, self.bail, &[], next, &[]);
        self.b.switch_to_block(next);
    }

    fn body(&mut self, f: &MirFunc) -> Result<(), String> {
        self.barriers = barrier_pairs(f);
        for blk in &f.blocks {
            for inst in &blk.insts {
                let array = match inst {
                    MirInst::Index { array, .. } | MirInst::ArrayLen { array, .. } => *array,
                    MirInst::StoreIndex { array, .. } => {
                        self.stored.insert(*array);
                        *array
                    }
                    _ => continue,
                };
                if !self.views.contains_key(&array) {
                    let vars = (
                        self.b.declare_var(types::I64),
                        self.b.declare_var(types::I64),
                        self.b.declare_var(types::I64),
                    );
                    self.views.insert(array, vars);
                }
            }
        }
        let entry = self.b.current_block().expect("entry block");
        let params: Vec<_> = self.b.block_params(entry)[1..].to_vec();
        for (&p, &x) in f.params.iter().zip(&params) {
            self.set(f, p, x);
            self.view_of(f, p);
        }
        for blk in &f.blocks {
            let cl = self.b.create_block();
            self.blocks.insert(blk.id, cl);
        }
        // Stack budget: bail before the native stack runs out.
        let sp = self.b.ins().get_stack_pointer(types::I64);
        let limit = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_STACK_LIMIT);
        let low = self.b.ins().icmp(IntCC::UnsignedLessThan, sp, limit);
        self.bail_if(low);
        let first = self.blocks[&f.entry];
        self.b.ins().jump(first, &[]);

        for blk in &f.blocks {
            let cl = self.blocks[&blk.id];
            self.b.switch_to_block(cl);
            for inst in &blk.insts {
                self.inst(f, inst)?;
            }
            self.term(f, blk.id, blk.term.as_ref().ok_or("no terminator")?)?;
        }

        // Bail: flag the context, return a dummy word.
        self.b.switch_to_block(self.bail);
        let one = self.b.ins().iconst(types::I64, 1);
        self.b.ins().store(MemFlagsData::trusted(), one, self.ctx, NATIVE_CTX_BAIL);
        let zero = if self.ret == types::F64 {
            self.b.ins().f64const(0.0)
        } else {
            self.b.ins().iconst(types::I64, 0)
        };
        self.b.ins().return_(&[zero]);
        self.b.seal_all_blocks();
        Ok(())
    }

    fn inst(&mut self, f: &MirFunc, inst: &MirInst) -> Result<(), String> {
        match inst {
            MirInst::Const { dest, c } => {
                let x = match *c {
                    MirConst::I64(v) => self.b.ins().iconst(types::I64, v),
                    MirConst::F64(bits) => self.b.ins().f64const(f64::from_bits(bits)),
                    MirConst::Bool(v) => self.b.ins().iconst(types::I64, i64::from(v)),
                };
                self.set(f, *dest, x);
            }
            MirInst::Bin { dest, op, ty, lhs, rhs } => {
                let a = self.get(f, *lhs);
                let c = self.get(f, *rhs);
                let x = if *ty == MirTy::F64 { self.fbin(*op, a, c)? } else { self.ibin(*op, a, c)? };
                self.set(f, *dest, x);
            }
            MirInst::Cmp { dest, op, ty, lhs, rhs } => {
                let a = self.get(f, *lhs);
                let c = self.get(f, *rhs);
                let t = if *ty == MirTy::F64 {
                    let cc = match op {
                        MirCmpOp::Lt => FloatCC::LessThan,
                        MirCmpOp::Le => FloatCC::LessThanOrEqual,
                        MirCmpOp::Gt => FloatCC::GreaterThan,
                        MirCmpOp::Ge => FloatCC::GreaterThanOrEqual,
                        MirCmpOp::Eq => FloatCC::Equal,
                        // The VM's `!=` is Rust's: true when unordered.
                        MirCmpOp::Ne => FloatCC::NotEqual,
                    };
                    self.b.ins().fcmp(cc, a, c)
                } else {
                    let cc = match op {
                        MirCmpOp::Lt => IntCC::SignedLessThan,
                        MirCmpOp::Le => IntCC::SignedLessThanOrEqual,
                        MirCmpOp::Gt => IntCC::SignedGreaterThan,
                        MirCmpOp::Ge => IntCC::SignedGreaterThanOrEqual,
                        MirCmpOp::Eq => IntCC::Equal,
                        MirCmpOp::Ne => IntCC::NotEqual,
                    };
                    self.b.ins().icmp(cc, a, c)
                };
                let x = self.b.ins().uextend(types::I64, t);
                self.set(f, *dest, x);
            }
            MirInst::Unary { dest, op, src } => {
                let a = self.get(f, *src);
                let x = match (op, ty_of(f, *src)) {
                    (MirUnaryOp::Neg, MirTy::F64) => self.b.ins().fneg(a),
                    (MirUnaryOp::Neg, _) => {
                        let min = self.b.ins().icmp_imm_s(IntCC::Equal, a, i64::MIN);
                        self.bail_if(min);
                        self.b.ins().ineg(a)
                    }
                    (MirUnaryOp::Not, MirTy::F64) => return Err("float not".into()),
                    (MirUnaryOp::Not, _) => {
                        let z = self.b.ins().icmp_imm_s(IntCC::Equal, a, 0);
                        self.b.ins().uextend(types::I64, z)
                    }
                };
                self.set(f, *dest, x);
            }
            MirInst::Cast { dest, kind, to, src } => {
                let a = self.get(f, *src);
                let from = ty_of(f, *src);
                let x = match kind {
                    MirCastKind::IntToFloat => self.b.ins().fcvt_from_sint(types::F64, a),
                    // Rust `as`: saturating, NaN is 0, as the VM.
                    MirCastKind::FloatToInt => self.b.ins().fcvt_to_sint_sat(types::I64, a),
                    MirCastKind::Word => {
                        let (ft, tt) = (cl_ty(from), cl_ty(*to));
                        if ft == tt { a } else { self.b.ins().bitcast(tt, MemFlagsData::new(), a) }
                    }
                };
                self.set(f, *dest, x);
            }
            MirInst::Phi { dest, .. } => self.view_of(f, *dest),
            MirInst::Deopt { .. } => {}
            MirInst::Call { dest, target, args, .. } => {
                let callee = *self.by_label.get(target).ok_or("unknown callee")?;
                let fref = *self.module_refs.get(&callee).ok_or("callee not compiled")?;
                let sig = self.b.func.dfg.ext_funcs[fref].signature;
                let params: Vec<Type> = self.b.func.dfg.signatures[sig].params.iter().map(|p| p.value_type).collect();
                let mut vals = vec![self.ctx];
                for (a, &want) in args.iter().zip(&params[1..]) {
                    let x = self.get(f, *a);
                    vals.push(self.coerce(x, want));
                }
                let call = self.b.ins().call(fref, &vals);
                let r = self.b.inst_results(call)[0];
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag);
                self.set(f, *dest, r);
            }
            MirInst::Index { dest, array, index, .. } => {
                let a = self.get(f, *array);
                let i = self.get(f, *index);
                let (fast, slow, join) = self.in_view(*array, i, true);
                self.b.switch_to_block(fast);
                let at = self.element(*array, i);
                let w = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
                self.b.ins().jump(join, &[w.into()]);
                self.b.switch_to_block(slow);
                let call = self.b.ins().call(self.index, &[self.ctx, a, i]);
                let w = self.b.inst_results(call)[0];
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag);
                self.b.ins().jump(join, &[w.into()]);
                self.b.switch_to_block(join);
                let w = self.b.block_params(join)[0];
                self.set_word(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::ArrayLen { dest, array } => {
                let a = self.get(f, *array);
                let (ok, _, len) = self.views[array];
                let ok = self.b.use_var(ok);
                let fast = self.b.create_block();
                let slow = self.b.create_block();
                let join = self.b.create_block();
                self.b.append_block_param(join, types::I64);
                self.b.ins().brif(ok, fast, &[], slow, &[]);
                self.b.switch_to_block(fast);
                let n = self.b.use_var(len);
                self.b.ins().jump(join, &[n.into()]);
                self.b.switch_to_block(slow);
                let call = self.b.ins().call(self.len, &[a]);
                let w = self.b.inst_results(call)[0];
                self.b.ins().jump(join, &[w.into()]);
                self.b.switch_to_block(join);
                let w = self.b.block_params(join)[0];
                self.set(f, *dest, w);
            }
            MirInst::HeapFieldLoad { dest, object, index } => {
                let o = self.get(f, *object);
                let i = self.b.ins().iconst(types::I64, i64::from(*index));
                let call = self.b.ins().call(self.field, &[o, i]);
                let w = self.b.inst_results(call)[0];
                self.set_word(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::StoreIndex { dest, array, index, value, .. } => {
                let a = self.get(f, *array);
                let i = self.get(f, *index);
                let v = self.get(f, *value);
                let v = self.coerce(v, types::I64);
                let (fast, slow, join) = self.in_view(*array, i, false);
                // In place: log `[array, index, old]`, then store.
                self.b.switch_to_block(fast);
                let at = self.element(*array, i);
                let old = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
                let t = MemFlagsData::trusted();
                let n = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_UNDO_LEN);
                let cap = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_UNDO_CAP);
                let full = self.b.ins().icmp(IntCC::Equal, n, cap);
                let grow = self.b.create_block();
                let write = self.b.create_block();
                self.b.ins().brif(full, grow, &[], write, &[]);
                self.b.switch_to_block(grow);
                self.b.ins().call(self.undo_grow, &[self.ctx]);
                self.b.ins().jump(write, &[]);
                self.b.switch_to_block(write);
                let buf = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_UNDO_BUF);
                let off = self.b.ins().imul_imm_u(n, 24);
                let e = self.b.ins().iadd(buf, off);
                self.b.ins().store(t, a, e, 0);
                self.b.ins().store(t, i, e, 8);
                self.b.ins().store(t, old, e, 16);
                let n1 = self.b.ins().iadd_imm_u(n, 1);
                self.b.ins().store(t, n1, self.ctx, NATIVE_CTX_UNDO_LEN);
                self.b.ins().store(t, v, at, 0);
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(slow);
                self.b.ins().call(self.store_index, &[self.ctx, a, i, v]);
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag);
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(join);
                let v = self.get(f, *value);
                self.set(f, *dest, v);
            }
            MirInst::HeapFieldStore { dest, object, value, index } => {
                let o = self.get(f, *object);
                let i = self.b.ins().iconst(types::I64, i64::from(*index));
                let v = self.get(f, *value);
                let v = self.coerce(v, types::I64);
                let call = self.b.ins().call(self.set_field, &[self.ctx, o, i, v]);
                let w = self.b.inst_results(call)[0];
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag);
                self.set(f, *dest, w);
            }
            MirInst::Alloc { dest, kind, elems, kinds } => {
                use machine::native::{ALLOC_ARRAY, ALLOC_ENUM, ALLOC_OBJECT, ALLOC_TUPLE};
                // Declaration order: an enum's elems are in push order and
                // its payload starts with the last push, as `DenseMake`.
                let mut words = elems.clone();
                if matches!(kind, MirAllocKind::Enum { .. }) {
                    words.reverse();
                }
                let packed = || common::pack_word_kinds(words.iter().map(|w| word_kind(ty_of(f, *w))));
                let (code, n, extra) = match *kind {
                    MirAllocKind::Array => (ALLOC_ARRAY, words.len() as u64, u64::from(*kinds)),
                    MirAllocKind::Tuple => {
                        (ALLOC_TUPLE, words.len() as u64, u64::from(if *kinds != 0 { *kinds } else { packed() }))
                    }
                    MirAllocKind::Enum { tag } => {
                        let k = if *kinds != 0 { *kinds } else { packed() };
                        (ALLOC_ENUM, words.len() as u64, u64::from(tag) | (u64::from(k) << 32))
                    }
                    MirAllocKind::Object { type_id, nfields } => (ALLOC_OBJECT, u64::from(nfields), u64::from(type_id)),
                    other => return Err(format!("allocation {other:?}")),
                };
                let slot = self.b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    (words.len().max(1) * 8) as u32,
                    3,
                ));
                for (i, w) in words.iter().enumerate() {
                    let x = self.get(f, *w);
                    let x = self.coerce(x, types::I64);
                    self.b.ins().stack_store(types::I64, x, slot, (i * 8) as i32);
                }
                let p = self.b.ins().stack_addr(types::I64, slot, 0);
                let code = self.b.ins().iconst(types::I64, code as i64);
                let n = self.b.ins().iconst(types::I64, n as i64);
                let extra = self.b.ins().iconst(types::I64, extra as i64);
                let call = self.b.ins().call(self.alloc, &[self.ctx, code, p, n, extra]);
                let w = self.b.inst_results(call)[0];
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag);
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::ArrayPush { dest, array, value } => {
                let a = self.get(f, *array);
                let v = self.get(f, *value);
                let v = self.coerce(v, types::I64);
                let call = self.b.ins().call(self.array_push, &[self.ctx, a, v]);
                let w = self.b.inst_results(call)[0];
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag);
                // A push may move the array's storage, and other values may
                // name the same array: every view reads through the helpers
                // until its value is defined again.
                let views: Vec<_> = self.views.values().map(|v| v.0).collect();
                let zero = self.b.ins().iconst(types::I64, 0);
                for ok in views {
                    self.b.def_var(ok, zero);
                }
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::MatchPayload { dest, scrutinee, index } => {
                let s = self.get(f, *scrutinee);
                let w = self.payload(f, *scrutinee, s, *index);
                self.set_word(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::GcBarrier { dest, .. } => {
                let obj = *self.barriers.get(dest).ok_or("GcBarrier without an allocation")?;
                let w = self.get(f, obj);
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::Cow { dest, src, unique, field } => {
                let s = self.get(f, *src);
                let s = self.coerce(s, types::I64);
                let call = match field {
                    Some(i) => {
                        let i = self.b.ins().iconst(types::I64, i64::from(*i));
                        self.b.ins().call(self.cow_field, &[self.ctx, s, i])
                    }
                    None => {
                        let u = self.b.ins().iconst(types::I64, i64::from(*unique));
                        self.b.ins().call(self.cow, &[self.ctx, s, u])
                    }
                };
                let w = self.b.inst_results(call)[0];
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag);
                self.set_word(f, *dest, w);
                self.view_of(f, *dest);
            }
            other => return Err(format!("instruction {}", inst_name(other))),
        }
        Ok(())
    }

    /// Payload word `index` of the matched `scrutinee` (word `s`): a niche
    /// word is its own payload, as the VM's emission moves it; a boxed
    /// enum's comes from its object.
    fn payload(&mut self, f: &MirFunc, scrutinee: ValueId, s: cranelift_codegen::ir::Value, index: u32) -> cranelift_codegen::ir::Value {
        let s = self.coerce(s, types::I64);
        if matches!(ty_of(f, scrutinee), MirTy::NicheOpt | MirTy::NicheRes) {
            return s;
        }
        let i = self.b.ins().iconst(types::I64, i64::from(index));
        let call = self.b.ins().call(self.enum_payload, &[self.ctx, s, i]);
        let w = self.b.inst_results(call)[0];
        let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
        self.bail_if(flag);
        w
    }

    /// Fill `v`'s view right after `v` is defined, when the body indexes it.
    fn view_of(&mut self, f: &MirFunc, v: ValueId) {
        let Some(&(ok, ptr, len)) = self.views.get(&v) else {
            return;
        };
        let a = self.get(f, v);
        let store = self.b.ins().iconst(types::I64, i64::from(self.stored.contains(&v)));
        let out = self.b.ins().stack_addr(types::I64, self.view_slot, 0);
        let call = self.b.ins().call(self.view, &[a, store, out]);
        let r = self.b.inst_results(call)[0];
        let p = self.b.ins().stack_load(types::I64, types::I64, self.view_slot, 0);
        let n = self.b.ins().stack_load(types::I64, types::I64, self.view_slot, 8);
        self.b.def_var(ok, r);
        self.b.def_var(ptr, p);
        self.b.def_var(len, n);
    }

    /// Branch on whether `array[i]` is in its view: `(in view, otherwise,
    /// join)`. The join block takes one word when the caller passes one.
    fn in_view(&mut self, array: ValueId, i: cranelift_codegen::ir::Value, word: bool) -> (Block, Block, Block) {
        let (ok, _, len) = self.views[&array];
        let ok = self.b.use_var(ok);
        let len = self.b.use_var(len);
        let inb = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        let has = self.b.ins().icmp_imm_s(IntCC::NotEqual, ok, 0);
        let c = self.b.ins().band(inb, has);
        let fast = self.b.create_block();
        let slow = self.b.create_block();
        let join = self.b.create_block();
        if word {
            self.b.append_block_param(join, types::I64);
        }
        self.b.ins().brif(c, fast, &[], slow, &[]);
        (fast, slow, join)
    }

    /// Address of `array[i]` in its view.
    fn element(&mut self, array: ValueId, i: cranelift_codegen::ir::Value) -> cranelift_codegen::ir::Value {
        let (_, ptr, _) = self.views[&array];
        let p = self.b.use_var(ptr);
        let off = self.b.ins().ishl_imm_u(i, 3);
        self.b.ins().iadd(p, off)
    }

    /// Define `v` from a VM word read out of the heap: a `bool` word is
    /// true when its low byte is `1`, as `Value::as_bool` reads it.
    fn set_word(&mut self, f: &MirFunc, v: ValueId, w: cranelift_codegen::ir::Value) {
        let w = if ty_of(f, v) == MirTy::Bool {
            let low = self.b.ins().band_imm_u(w, 0xff);
            let t = self.b.ins().icmp_imm_s(IntCC::Equal, low, 1);
            self.b.ins().uextend(types::I64, t)
        } else {
            w
        };
        self.set(f, v, w);
    }

    fn ibin(&mut self, op: MirBinOp, a: cranelift_codegen::ir::Value, c: cranelift_codegen::ir::Value) -> Result<cranelift_codegen::ir::Value, String> {
        Ok(match op {
            MirBinOp::Add => {
                let (r, of) = self.b.ins().sadd_overflow(a, c);
                self.bail_if(of);
                r
            }
            MirBinOp::Sub => {
                let (r, of) = self.b.ins().ssub_overflow(a, c);
                self.bail_if(of);
                r
            }
            MirBinOp::Mul => {
                let (r, of) = self.b.ins().smul_overflow(a, c);
                self.bail_if(of);
                r
            }
            MirBinOp::Div => {
                let z = self.b.ins().icmp_imm_s(IntCC::Equal, c, 0);
                self.bail_if(z);
                let m1 = self.b.ins().icmp_imm_s(IntCC::Equal, c, -1);
                let min = self.b.ins().icmp_imm_s(IntCC::Equal, a, i64::MIN);
                let both = self.b.ins().band(m1, min);
                self.bail_if(both);
                self.b.ins().sdiv(a, c)
            }
            MirBinOp::Rem => {
                let z = self.b.ins().icmp_imm_s(IntCC::Equal, c, 0);
                self.bail_if(z);
                // `x % -1` is 0 (and `srem` would trap on `MIN % -1`).
                let m1 = self.b.ins().icmp_imm_s(IntCC::Equal, c, -1);
                let one = self.b.ins().iconst(types::I64, 1);
                let safe = self.b.ins().select(m1, one, c);
                let r = self.b.ins().srem(a, safe);
                let zero = self.b.ins().iconst(types::I64, 0);
                self.b.ins().select(m1, zero, r)
            }
            MirBinOp::BitAnd => self.b.ins().band(a, c),
            MirBinOp::BitOr => self.b.ins().bor(a, c),
            MirBinOp::Xor => self.b.ins().bxor(a, c),
            MirBinOp::Shl | MirBinOp::Shr => {
                let out = self.b.ins().icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, c, 64);
                self.bail_if(out);
                if op == MirBinOp::Shl { self.b.ins().ishl(a, c) } else { self.b.ins().sshr(a, c) }
            }
            MirBinOp::Pow => return Err("int **".into()),
        })
    }

    fn fbin(&mut self, op: MirBinOp, a: cranelift_codegen::ir::Value, c: cranelift_codegen::ir::Value) -> Result<cranelift_codegen::ir::Value, String> {
        Ok(match op {
            MirBinOp::Add => self.b.ins().fadd(a, c),
            MirBinOp::Sub => self.b.ins().fsub(a, c),
            MirBinOp::Mul => self.b.ins().fmul(a, c),
            MirBinOp::Div => self.b.ins().fdiv(a, c),
            MirBinOp::Rem | MirBinOp::Pow => {
                let f = if op == MirBinOp::Rem { self.fmod } else { self.powf };
                let call = self.b.ins().call(f, &[a, c]);
                self.b.inst_results(call)[0]
            }
            _ => return Err("float bit op".into()),
        })
    }

    /// Assign the φs of `to` for the edge from `from`.
    fn edge_phis(&mut self, f: &MirFunc, from: BlockId, to: BlockId) -> Result<(), String> {
        let blk = f.blocks.iter().find(|b| b.id == to).ok_or("missing block")?;
        let mut moves = Vec::new();
        for inst in &blk.insts {
            if let MirInst::Phi { dest, args, .. } = inst {
                let (_, v) = args.iter().find(|(p, _)| *p == from).ok_or("φ without this edge")?;
                moves.push((*dest, *v));
            }
        }
        // Read every source first: one φ may read another φ of this block.
        let vals: Vec<_> = moves.iter().map(|&(_, v)| self.get(f, v)).collect();
        for ((dest, _), x) in moves.into_iter().zip(vals) {
            self.set(f, dest, x);
        }
        Ok(())
    }

    fn term(&mut self, f: &MirFunc, here: BlockId, t: &Terminator) -> Result<(), String> {
        match t {
            Terminator::Jump { dest } => {
                self.edge_phis(f, here, *dest)?;
                let to = self.blocks[dest];
                self.b.ins().jump(to, &[]);
            }
            Terminator::Br { cond, taken, not_taken } => {
                let c = self.get(f, *cond);
                // One block per edge, so a φ move on one edge never
                // reaches the other successor.
                let et = self.b.create_block();
                let en = self.b.create_block();
                self.b.ins().brif(c, et, &[], en, &[]);
                for (edge, dest) in [(et, *taken), (en, *not_taken)] {
                    self.b.switch_to_block(edge);
                    self.edge_phis(f, here, dest)?;
                    let to = self.blocks[&dest];
                    self.b.ins().jump(to, &[]);
                }
            }
            Terminator::Return { lo: Some(v), hi: None } => {
                let x = self.get(f, *v);
                let x = self.coerce(x, self.ret);
                self.b.ins().return_(&[x]);
            }
            Terminator::Unreachable => {
                self.b.ins().jump(self.bail, &[]);
            }
            Terminator::JumpIfMatch { scrutinee, tag, payloads, taken, not_taken } => {
                let s = self.get(f, *scrutinee);
                let et = self.b.create_block();
                let en = self.b.create_block();
                match ty_of(f, *scrutinee) {
                    // `None` is 0 (tag 0); `Some` any other word (tag 1).
                    MirTy::NicheOpt => {
                        let none = self.b.ins().icmp_imm_s(IntCC::Equal, s, 0);
                        let (t, n) = if *tag == 0 { (et, en) } else { (en, et) };
                        self.b.ins().brif(none, t, &[], n, &[]);
                    }
                    // `Err` has the low bit set (tag 1); `Ok` is aligned (tag 0).
                    MirTy::NicheRes => {
                        let err = self.b.ins().band_imm_u(s, 1);
                        let (t, n) = if *tag == 1 { (et, en) } else { (en, et) };
                        self.b.ins().brif(err, t, &[], n, &[]);
                    }
                    _ => {
                        // A boxed enum: `tag | payload length << 32`. A tag
                        // match with another payload width than the arm
                        // binds is left to the VM.
                        let call = self.b.ins().call(self.enum_tag, &[s]);
                        let tl = self.b.inst_results(call)[0];
                        let not_enum = self.b.ins().icmp_imm_s(IntCC::Equal, tl, -1);
                        self.bail_if(not_enum);
                        let low = self.b.ins().band_imm_u(tl, 0xffff_ffff);
                        let hit = self.b.ins().icmp_imm_s(IntCC::Equal, low, i64::from(*tag));
                        let check = self.b.create_block();
                        self.b.ins().brif(hit, check, &[], en, &[]);
                        self.b.switch_to_block(check);
                        let n = self.b.ins().ushr_imm_u(tl, 32);
                        let other = self.b.ins().icmp_imm_s(IntCC::NotEqual, n, payloads.len() as i64);
                        self.bail_if(other);
                        self.b.ins().jump(et, &[]);
                    }
                }
                self.b.switch_to_block(et);
                for (i, p) in payloads.iter().enumerate() {
                    let w = self.payload(f, *scrutinee, s, i as u32);
                    self.set_word(f, *p, w);
                    self.view_of(f, *p);
                }
                self.edge_phis(f, here, *taken)?;
                let to = self.blocks[taken];
                self.b.ins().jump(to, &[]);
                self.b.switch_to_block(en);
                self.edge_phis(f, here, *not_taken)?;
                let to = self.blocks[not_taken];
                self.b.ins().jump(to, &[]);
            }
            _ => return Err("terminator".into()),
        }
        Ok(())
    }
}

/// Body names compiled per call to [`jit`], for `COIL_NATIVE_STATS`.
pub fn describe(p: &NativeProgram) -> String {
    let refused: HashSet<&str> = p.refused.iter().map(|(n, _)| n.as_str()).collect();
    format!(
        "native: {} compiled, {} installed, {} on the VM",
        p.compiled.len(),
        p.entries.len(),
        refused.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use compiler::native_mir::{MirBlock, Terminator};
    use machine::native::{NATIVE_STACK_BUDGET, NativeCtx};

    fn run(entry: &NativeEntry, args: &[u64]) -> (u64, u64) {
        let marker = 0u8;
        let mut undo = Vec::new();
        let mut ctx = NativeCtx::new((std::ptr::addr_of!(marker) as u64).saturating_sub(NATIVE_STACK_BUDGET), &mut undo);
        // SAFETY: compiled for `args.len()` argument words.
        let out = unsafe { (entry.entry)(&mut ctx, args.as_ptr()) };
        (ctx.bail, out)
    }

    /// `ret_ty` body returning its one parameter of type `param`.
    fn ident(name: &str, param: MirTy, ret: MirTy) -> MirFunc {
        let mut f = MirFunc::new(name);
        f.blocks.clear();
        f.types = vec![param];
        f.params = vec![ValueId(0)];
        f.ret_ty = Some(ret);
        let mut b = MirBlock::new(BlockId(0));
        b.term = Some(Terminator::Return { lo: Some(ValueId(0)), hi: None });
        f.blocks.push(b);
        f
    }

    #[test]
    fn a_float_result_moves_into_an_int_value_by_bits() {
        let g = ident("g", MirTy::F64, MirTy::F64);
        let mut f = MirFunc::new("f");
        f.blocks.clear();
        f.types = vec![MirTy::F64, MirTy::I64];
        f.params = vec![ValueId(0)];
        f.ret_ty = Some(MirTy::I64);
        let mut b = MirBlock::new(BlockId(0));
        b.insts.push(MirInst::Call { dest: ValueId(1), dest_hi: None, target: Label(1), args: vec![ValueId(0)] });
        b.term = Some(Terminator::Return { lo: Some(ValueId(1)), hi: None });
        f.blocks.push(b);
        let bodies = vec![
            NativeBody { name: "f".into(), entry: Some(Label(0)), mir: f },
            NativeBody { name: "g".into(), entry: Some(Label(1)), mir: g },
        ];
        let pcs = HashMap::from([("f".to_string(), 0u32)]);
        let p = jit(&bodies, &pcs).expect("jit");
        assert_eq!(p.entries.len(), 1, "refused: {:?}", p.refused);
        assert_eq!(run(&p.entries[0], &[1.5f64.to_bits()]), (0, 1.5f64.to_bits()));
    }

    #[test]
    fn a_callee_that_fails_to_lower_keeps_its_callers_on_the_vm() {
        // `g` passes `plan` but has no lowering (`!` of a float); `f` calls it.
        let mut g = MirFunc::new("g");
        g.blocks.clear();
        g.types = vec![MirTy::F64, MirTy::Bool];
        g.params = vec![ValueId(0)];
        g.ret_ty = Some(MirTy::Bool);
        let mut b = MirBlock::new(BlockId(0));
        b.insts.push(MirInst::Unary { dest: ValueId(1), op: MirUnaryOp::Not, src: ValueId(0) });
        b.term = Some(Terminator::Return { lo: Some(ValueId(1)), hi: None });
        g.blocks.push(b);
        let mut f = MirFunc::new("f");
        f.blocks.clear();
        f.types = vec![MirTy::F64, MirTy::Bool];
        f.params = vec![ValueId(0)];
        f.ret_ty = Some(MirTy::Bool);
        let mut b = MirBlock::new(BlockId(0));
        b.insts.push(MirInst::Call { dest: ValueId(1), dest_hi: None, target: Label(1), args: vec![ValueId(0)] });
        b.term = Some(Terminator::Return { lo: Some(ValueId(1)), hi: None });
        f.blocks.push(b);
        let h = ident("h", MirTy::I64, MirTy::I64);
        let bodies = vec![
            NativeBody { name: "f".into(), entry: Some(Label(0)), mir: f },
            NativeBody { name: "g".into(), entry: Some(Label(1)), mir: g },
            NativeBody { name: "h".into(), entry: Some(Label(2)), mir: h },
        ];
        let pcs = HashMap::from([("f".to_string(), 0u32), ("h".to_string(), 1u32)]);
        let p = jit(&bodies, &pcs).expect("jit");
        assert_eq!(p.compiled, vec!["h".to_string()]);
        assert_eq!(run(&p.entries[0], &[7]), (0, 7));
    }
}
