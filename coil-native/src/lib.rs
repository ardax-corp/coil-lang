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
    BlockId, Label, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirFunc, MirInst, MirTy, MirUnaryOp,
    NativeBody, Terminator, ValueId,
};
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{AbiParam, Block, FuncRef, InstBuilder, MemFlagsData, Signature, Type, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use machine::native::{NATIVE_CTX_BAIL, NATIVE_CTX_STACK_LIMIT, NativeEntry, NativeEntryFn};

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
    let plan = plan(bodies);
    let mut refused = plan.refused;
    if plan.compile.is_empty() {
        return Ok(NativeProgram { entries: Vec::new(), compiled: Vec::new(), refused });
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
            refused.push((body.name.clone(), why));
            module.clear_context(&mut ctx);
            continue;
        }
        if let Err(e) = module.define_function(ids.imp[&i], &mut ctx) {
            refused.push((body.name.clone(), format!("cranelift: {e:?}")));
            module.clear_context(&mut ctx);
            continue;
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
    Ok(NativeProgram { entries, compiled, refused })
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
    let by_label: HashMap<Label, usize> = bodies
        .iter()
        .enumerate()
        .filter_map(|(i, b)| Some((b.entry?, i)))
        .collect();
    let mut refused = Vec::new();
    let mut ok: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, b) in bodies.iter().enumerate() {
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

fn numeric(t: MirTy) -> bool {
    matches!(t, MirTy::I64 | MirTy::F64 | MirTy::Bool)
}

fn ty_of(f: &MirFunc, v: ValueId) -> MirTy {
    f.types.get(v.index()).copied().unwrap_or(MirTy::Bottom)
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
    let need = |v: &ValueId| -> Result<(), String> {
        let t = ty_of(f, *v);
        if numeric(t) { Ok(()) } else { Err(format!("value type {t:?}")) }
    };
    for b in &f.blocks {
        for inst in &b.insts {
            match inst {
                MirInst::Const { dest, .. } => need(dest)?,
                MirInst::Bin { dest, op, ty, lhs, rhs } => {
                    need(dest)?;
                    need(lhs)?;
                    need(rhs)?;
                    if *ty == MirTy::I64 && *op == MirBinOp::Pow {
                        return Err("int **".into());
                    }
                    if *ty == MirTy::Bool && !op.requires_int() {
                        return Err("bool arithmetic".into());
                    }
                }
                MirInst::Cmp { dest, lhs, rhs, .. } => {
                    need(dest)?;
                    need(lhs)?;
                    need(rhs)?;
                }
                MirInst::Unary { dest, src, .. } => {
                    need(dest)?;
                    need(src)?;
                }
                MirInst::Cast { dest, src, .. } => {
                    need(dest)?;
                    need(src)?;
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
                MirInst::Deopt { .. } => {}
                other => return Err(format!("instruction {}", inst_name(other))),
            }
        }
        match &b.term {
            Some(Terminator::Jump { .. }) | Some(Terminator::Unreachable) => {}
            Some(Terminator::Br { cond, .. }) => need(cond)?,
            Some(Terminator::Return { lo: Some(v), hi: None }) => need(v)?,
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
    Ok(Ids { imp, entry, fmod, powf })
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
        self.b.def_var(var, x);
    }

    /// Branch to the bail block when `cond` is non-zero; continue in a new block.
    fn bail_if(&mut self, cond: cranelift_codegen::ir::Value) {
        let next = self.b.create_block();
        self.b.ins().brif(cond, self.bail, &[], next, &[]);
        self.b.switch_to_block(next);
    }

    fn body(&mut self, f: &MirFunc) -> Result<(), String> {
        let entry = self.b.current_block().expect("entry block");
        let params: Vec<_> = self.b.block_params(entry)[1..].to_vec();
        for (&p, &x) in f.params.iter().zip(&params) {
            self.set(f, p, x);
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
            MirInst::Phi { .. } | MirInst::Deopt { .. } => {}
            MirInst::Call { dest, target, args, .. } => {
                let callee = *self.by_label.get(target).ok_or("unknown callee")?;
                let fref = *self.module_refs.get(&callee).ok_or("callee not compiled")?;
                let mut vals = vec![self.ctx];
                for a in args {
                    vals.push(self.get(f, *a));
                }
                let call = self.b.ins().call(fref, &vals);
                let r = self.b.inst_results(call)[0];
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag);
                self.set(f, *dest, r);
            }
            other => return Err(format!("instruction {}", inst_name(other))),
        }
        Ok(())
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
                self.b.ins().return_(&[x]);
            }
            Terminator::Unreachable => {
                self.b.ins().jump(self.bail, &[]);
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
