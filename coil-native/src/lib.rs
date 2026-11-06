//! Native code for Coil: typed MIR to machine code with Cranelift.
//!
//! See `docs/internals/native.md`. A body compiles when every call it makes
//! goes to another body that compiles. A trap panics natively (see
//! `machine::native`).
//!
//! [`jit`] compiles in memory for `coil FILE`. An object-file driver for
//! `coil package` reuses the same lowering later.
//!
//! Empty on wasm: the playground has no native code and runs the MIR
//! interpreter.
#![cfg(not(target_family = "wasm"))]

mod image;
pub mod section;

pub use image::load as load_image;

use std::collections::{HashMap, HashSet};

use compiler::native_mir::{
    BlockId, Label, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirFunc, MirInst, MirTy, MirUnaryOp,
    NativeBody, Terminator, ValueId, allocates, barrier_pairs, heap_word, held_words, is_generator, numeric, opaque_words, pair_words, pointer_words, scalar,
    ty_of, root_handles, used_params, word_kind,
};
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::ir::{AbiParam, Block, FuncRef, InstBuilder, MemFlagsData, Signature, Type, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use machine::native::{
    NATIVE_CTX_BAIL, NATIVE_CTX_RET_HI, NATIVE_CTX_ROOTS, NATIVE_CTX_GC_DUE, NATIVE_CTX_GC_EPOCH, NATIVE_CTX_HAS_FINALIZERS, NATIVE_CTX_STATICS, NATIVE_CTX_STATICS_LEN, NATIVE_CTX_STACK_LIMIT, NATIVE_CTX_CALLS, ROOTS_AMBIGUOUS,
    NativeEntry, NativeEntryFn, NativeSite, NO_SITE, TRAP_DIV_ZERO, TRAP_HELPER, TRAP_OVERFLOW, TRAP_SHIFT, TRAP_STACK,
};

/// A root record [`Lower::push_roots`] linked.
struct Roots {
    prev: cranelift_codegen::ir::Value,
    slot: cranelift_codegen::ir::StackSlot,
    /// Heap homes the record lists, written back from it.
    homes: Vec<u16>,
    /// `NativeCtx::gc_epoch` when it was linked.
    epoch: cranelift_codegen::ir::Value,
}

/// What [`jit`] produced.
pub struct NativeProgram {
    /// Entries to install with `machine::native::install_for_next_run`.
    pub entries: Vec<NativeEntry>,
    /// Bodies compiled to native code.
    pub compiled: Vec<String>,
    /// Bodies left to the runtime, with the reason.
    pub refused: Vec<(String, String)>,
    /// The panic sites the code names (see `machine::native::NativeSite`).
    pub sites: Vec<NativeSite>,
    /// The code as bytes for [`load_image`], when [`jit_keeping_image`] asked.
    pub image: Option<Vec<u8>>,
}

/// Compile every eligible body of `bodies` in memory. `entry_pcs` maps a
/// body's name to its entry id. The code stays mapped for the
/// rest of the process.
///
/// Under `COIL_CODE_IMAGE_CHECK` the program goes through a code image and
/// the run uses the copy, as a code cache's next run would.
pub fn jit(bodies: &[NativeBody], entry_pcs: &HashMap<String, u32>) -> Result<NativeProgram, String> {
    if std::env::var_os("COIL_CODE_IMAGE_CHECK").is_some() {
        let program = jit_keeping_image(bodies, entry_pcs)?;
        return match &program.image {
            Some(image) => image::load(image),
            // A refused body leaves native code incomplete, so no image.
            None if !program.refused.is_empty() => Ok(program),
            None => Err("the code did not fit a code image".to_string()),
        };
    }
    jit_with(bodies, entry_pcs, false)
}

/// [`jit`], with [`NativeProgram::image`] for [`load_image`] when every body
/// compiled and the code fits an image.
pub fn jit_keeping_image(bodies: &[NativeBody], entry_pcs: &HashMap<String, u32>) -> Result<NativeProgram, String> {
    jit_with(bodies, entry_pcs, true)
}

fn jit_with(bodies: &[NativeBody], entry_pcs: &HashMap<String, u32>, keep_image: bool) -> Result<NativeProgram, String> {
    // A body `plan` admits may still fail to lower; its callers already
    // name it, so leave it out and build the module again.
    let mut excluded = HashMap::new();
    loop {
        match jit_once(bodies, entry_pcs, &excluded, keep_image)? {
            Attempt::Done(program) => return Ok(program),
            Attempt::Without(i, why) => {
                excluded.insert(i, why);
            }
        }
    }
}

/// JIT an archive's native MIR section for the coming run, for a host with
/// no runtime to run what does not compile: every body must.
pub fn install_section(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("the program carries no native MIR; build it again with `coil package`".to_string());
    }
    let (bodies, pcs) = section::decode(bytes)?;
    let program = jit(&bodies, &pcs)?;
    if let Some((name, why)) = program.refused.first() {
        return Err(format!("`{name}` cannot compile to native code: {why}"));
    }
    if std::env::var_os("COIL_NATIVE_STATS").is_some() {
        eprintln!("{}", describe(&program));
    }
    machine::native::install_for_next_run(program.entries, program.sites, true);
    Ok(())
}

/// The [`machine::native::InlineShape`] of an `Alloc` of `kind` with `n`
/// words and the helper's `extra` word, when native code may allocate it
/// inline (`COIL_NATIVE_INLINE_ALLOC=0` turns that off, for A/B runs).
fn inline_shape(kind: MirAllocKind, n: usize, extra: u64) -> Option<machine::native::InlineShape> {
    use machine::native::InlineShape;
    if std::env::var_os("COIL_NATIVE_INLINE_ALLOC").is_some_and(|v| v == "0") {
        return None;
    }
    let n = u8::try_from(n).ok()?;
    match kind {
        MirAllocKind::Tuple => Some(InlineShape::Tuple { kinds: extra as u8, n }),
        MirAllocKind::Enum { tag } => Some(InlineShape::Enum { tag, kinds: (extra >> 32) as u8, n }),
        MirAllocKind::Object { type_id, nfields } => Some(InlineShape::Instance { type_id, n: u8::try_from(nfields).ok()? }),
        _ => None,
    }
}

/// What [`aot`] produced: a relocatable object for the host target that
/// defines the compiled bodies and the tables `machine::native::install_aot`
/// reads (`coil_aot_entries`, `coil_aot_entry_count`, `coil_aot_sites`,
/// `coil_aot_site_count`, `coil_aot_refused_count`, `coil_aot_layout`,
/// `coil_aot_stack_maps`, `coil_aot_stack_map_count`), importing the runtime helpers by name, and the
/// statically linked `extern` symbols' table `machine::install_static_symbols` reads
/// (`coil_static_syms`, `coil_static_sym_count`).
pub struct AotObject {
    pub bytes: Vec<u8>,
    pub compiled: Vec<String>,
    pub refused: Vec<(String, String)>,
}

/// Compile every eligible body of `bodies` to an object file (`coil
/// package`): the same lowering as [`jit`], linked against the runtime
/// library instead of bound in memory. `statics` are the `extern`
/// functions linked into the executable, as `(library stem, C symbol)`:
/// the object imports each symbol and records its address. `package` is
/// the program's data (`common::linked_package`), as `coil_aot_package`.
pub fn aot(bodies: &[NativeBody], entry_pcs: &HashMap<String, u32>, statics: &[(String, String)], package: &[u8]) -> Result<AotObject, String> {
    let mut excluded = HashMap::new();
    loop {
        let plan = plan_without(bodies, &excluded, entry_pcs);
        let refused = plan.refused.clone();
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
        // Executables are position independent on most hosts.
        flags.set("is_pic", "true").map_err(|e| e.to_string())?;
        flags.set("preserve_frame_pointers", "true").map_err(|e| e.to_string())?;
        let isa = cranelift_native::builder()
            .map_err(|e| e.to_string())?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())?;
        let builder = cranelift_object::ObjectBuilder::new(isa, "coil_aot", default_libcall_names()).map_err(|e| e.to_string())?;
        let mut module = cranelift_object::ObjectModule::new(builder);
        let (ids, defined, sites, maps) = match define_all(&mut module, bodies, entry_pcs, &plan, None)? {
            Ok(done) => done,
            Err((i, why)) => {
                excluded.insert(i, why);
                continue;
            }
        };
        let e = |e: cranelift_module::ModuleError| e.to_string();
        // The entry table: `machine::native::AotEntry` records.
        const ENTRY_BYTES: usize = std::mem::size_of::<machine::native::AotEntry>();
        let mut table = cranelift_module::DataDescription::new();
        let mut bytes = Vec::new();
        let mut relocs = Vec::new();
        let mut compiled = Vec::new();
        for &i in &defined {
            let body = &bodies[i];
            compiled.push(body.name.clone());
            let Some(&pc) = entry_pcs.get(&body.name) else { continue };
            let at = bytes.len();
            bytes.resize(at + ENTRY_BYTES, 0);
            bytes[at..at + 4].copy_from_slice(&pc.to_le_bytes());
            bytes[at + 4..at + 6].copy_from_slice(&(body.mir.params.len() as u16).to_le_bytes());
            let reads = plan.used_params.get(&i).copied().unwrap_or(body.mir.params.len()) as u16;
            bytes[at + 6..at + 8].copy_from_slice(&reads.to_le_bytes());
            bytes[at + 8] = if body.mir.ret_hi_ty.is_some() { 2 } else { 1 };
            bytes[at + 9] = u8::from(plan.runtime_reach.contains(&i));
            bytes[at + std::mem::offset_of!(machine::native::AotEntry, generator)] = u8::from(is_generator(&body.mir));
            bytes[at + std::mem::offset_of!(machine::native::AotEntry, cleanup)] = u8::from(body.mir.cleanup.is_some());
            relocs.push((at + std::mem::offset_of!(machine::native::AotEntry, entry), ids.entry[&i]));
        }
        let count = bytes.len() / ENTRY_BYTES;
        if bytes.is_empty() {
            bytes.resize(ENTRY_BYTES, 0);
        }
        table.define(bytes.into_boxed_slice());
        table.set_align(8);
        for (off, id) in relocs {
            let f = module.declare_func_in_data(id, &mut table);
            table.write_function_addr(off as u32, f);
        }
        let table_id = module.declare_data("coil_aot_entries", Linkage::Export, false, false).map_err(e)?;
        module.define_data(table_id, &table).map_err(e)?;
        // The stack maps: `machine::native::AotStackMap` records, each
        // followed by its offsets, the function address relocated.
        let mut map_bytes = Vec::new();
        let mut map_relocs = Vec::new();
        let mut map_count = 0u64;
        for (id, (size, fmaps)) in &maps {
            map_relocs.push((map_bytes.len(), *id));
            map_bytes.extend_from_slice(&0u64.to_le_bytes());
            map_bytes.extend_from_slice(&size.to_le_bytes());
            map_bytes.extend_from_slice(&(fmaps.len() as u32).to_le_bytes());
            for (ra, offs) in fmaps {
                map_bytes.extend_from_slice(&ra.to_le_bytes());
                map_bytes.extend_from_slice(&(offs.len() as u32).to_le_bytes());
                for o in offs {
                    map_bytes.extend_from_slice(&o.to_le_bytes());
                }
            }
            map_bytes.resize(map_bytes.len().next_multiple_of(8), 0);
            map_count += 1;
        }
        if map_bytes.is_empty() {
            map_bytes.resize(16, 0);
        }
        let mut map_table = cranelift_module::DataDescription::new();
        map_table.define(map_bytes.into_boxed_slice());
        map_table.set_align(8);
        for (off, id) in map_relocs {
            let f = module.declare_func_in_data(id, &mut map_table);
            map_table.write_function_addr(off as u32, f);
        }
        let map_id = module.declare_data("coil_aot_stack_maps", Linkage::Export, false, false).map_err(e)?;
        module.define_data(map_id, &map_table).map_err(e)?;
        let mut data = cranelift_module::DataDescription::new();
        data.define(package.to_vec().into_boxed_slice());
        // Its archive is read in place, which rkyv's alignment asks of it.
        data.set_align(16);
        let id = module.declare_data("coil_aot_package", Linkage::Export, false, false).map_err(e)?;
        module.define_data(id, &data).map_err(e)?;
        let mut words = |name: &str, data: Vec<u8>| -> Result<(), String> {
            let mut d = cranelift_module::DataDescription::new();
            d.define(data.into_boxed_slice());
            d.set_align(8);
            let id = module.declare_data(name, Linkage::Export, false, false).map_err(e)?;
            module.define_data(id, &d).map_err(e)
        };
        words("coil_aot_entry_count", (count as u64).to_le_bytes().to_vec())?;
        words("coil_aot_package_len", (package.len() as u64).to_le_bytes().to_vec())?;
        let mut site_words = Vec::with_capacity(sites.len() * 16 + 16);
        for site in &sites {
            for w in [site.entry_pc, site.loc.file, site.loc.start_byte, site.loc.end_byte] {
                site_words.extend_from_slice(&w.to_le_bytes());
            }
        }
        let site_count = sites.len() as u64;
        if site_words.is_empty() {
            site_words.resize(16, 0);
        }
        words("coil_aot_sites", site_words)?;
        words("coil_aot_site_count", site_count.to_le_bytes().to_vec())?;
        words("coil_aot_refused_count", (refused.len() as u64).to_le_bytes().to_vec())?;
        words("coil_aot_layout", machine::native::layout_fingerprint().to_le_bytes().to_vec())?;
        words("coil_aot_stack_map_count", map_count.to_le_bytes().to_vec())?;
        static_symbol_table(&mut module, statics)?;
        let bytes = module.finish().emit().map_err(|e| e.to_string())?;
        return Ok(AotObject { bytes, compiled, refused });
    }
}

/// `coil_static_syms`: a `machine::StaticSymbol` record (library
/// stem, symbol, address) per statically linked `extern` function, and
/// `coil_static_sym_count`.
fn static_symbol_table(module: &mut cranelift_object::ObjectModule, statics: &[(String, String)]) -> Result<(), String> {
    let e = |e: cranelift_module::ModuleError| e.to_string();
    const RECORD: usize = std::mem::size_of::<machine::StaticSymbol>();
    let mut table = cranelift_module::DataDescription::new();
    table.define(vec![0u8; RECORD * statics.len().max(1)].into_boxed_slice());
    table.set_align(8);
    let mut strings = HashMap::new();
    let mut string = |module: &mut cranelift_object::ObjectModule, text: &str| -> Result<cranelift_module::DataId, String> {
        if let Some(&id) = strings.get(text) {
            return Ok(id);
        }
        let mut d = cranelift_module::DataDescription::new();
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        d.define(bytes.into_boxed_slice());
        let id = module.declare_anonymous_data(false, false).map_err(e)?;
        module.define_data(id, &d).map_err(e)?;
        strings.insert(text.to_string(), id);
        Ok(id)
    };
    let sig = module.make_signature();
    let mut funcs = HashMap::new();
    for (i, (lib, sym)) in statics.iter().enumerate() {
        let at = (i * RECORD) as u32;
        for (off, text) in [(std::mem::offset_of!(machine::StaticSymbol, library), lib), (std::mem::offset_of!(machine::StaticSymbol, symbol), sym)] {
            let id = string(module, text)?;
            let gv = module.declare_data_in_data(id, &mut table);
            table.write_data_addr(at + off as u32, gv, 0);
        }
        let f = match funcs.get(sym) {
            Some(&f) => f,
            None => {
                let id = module.declare_function(sym, Linkage::Import, &sig).map_err(e)?;
                let f = module.declare_func_in_data(id, &mut table);
                funcs.insert(sym.clone(), f);
                f
            }
        };
        table.write_function_addr(at + std::mem::offset_of!(machine::StaticSymbol, address) as u32, f);
    }
    let id = module.declare_data("coil_static_syms", Linkage::Export, false, false).map_err(e)?;
    module.define_data(id, &table).map_err(e)?;
    let mut d = cranelift_module::DataDescription::new();
    d.define((statics.len() as u64).to_le_bytes().to_vec().into_boxed_slice());
    d.set_align(8);
    let id = module.declare_data("coil_static_sym_count", Linkage::Export, false, false).map_err(e)?;
    module.define_data(id, &d).map_err(e)
}

enum Attempt {
    Done(NativeProgram),
    /// Body `i` does not lower: try again without it.
    Without(usize, String),
}

/// The ISA native code is compiled for here: what a code image is only good
/// for, along with the build that made it.
fn jit_isa() -> Result<cranelift_codegen::isa::OwnedTargetIsa, String> {
    let mut flags = settings::builder();
    flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
    // The collector walks native frames by their frame pointers.
    flags.set("preserve_frame_pointers", "true").map_err(|e| e.to_string())?;
    let isa_builder = cranelift_native::builder().map_err(|e| e.to_string())?;
    isa_builder.finish(settings::Flags::new(flags)).map_err(|e| e.to_string())
}

/// A module for JIT code, with `reserve` bytes of address space.
fn jit_module(reserve: usize) -> Result<JITModule, String> {
    let mut jb = JITBuilder::with_isa(jit_isa()?, default_libcall_names());
    for &(name, ptr) in machine::native::helper_symbols() {
        jb.symbol(name, ptr);
    }
    // One reservation: code reaches its read-only data by 32-bit offsets, and
    // the default provider maps each section where the system puts it, which
    // beside a process's big fiber stacks may be out of reach.
    if let Ok(arena) = cranelift_jit::ArenaMemoryProvider::new_with_size(reserve) {
        jb.memory_provider(Box::new(arena));
    }
    Ok(JITModule::new(jb))
}

/// What a code image depends on besides the program and the build: this
/// host's target and CPU features.
pub fn image_fingerprint() -> Result<String, String> {
    let isa = jit_isa()?;
    let flags: Vec<String> = isa.isa_flags().iter().map(ToString::to_string).collect();
    Ok(format!("{} {}", isa.triple(), flags.join(" ")))
}

fn jit_once(
    bodies: &[NativeBody],
    entry_pcs: &HashMap<String, u32>,
    excluded: &HashMap<usize, String>,
    keep_image: bool,
) -> Result<Attempt, String> {
    let plan = plan_without(bodies, excluded, entry_pcs);
    let refused = plan.refused.clone();
    if plan.compile.is_empty() {
        return Ok(Attempt::Done(NativeProgram { entries: Vec::new(), compiled: Vec::new(), refused, sites: Vec::new(), image: None }));
    }

    let reserve = jit_reserve(bodies, &plan);
    let mut module = jit_module(reserve)?;
    let mut recorder = keep_image.then(image::Recorder::default);

    let (ids, defined, sites, maps) = match define_all(&mut module, bodies, entry_pcs, &plan, recorder.as_mut())? {
        Ok(done) => done,
        Err((i, why)) => return Ok(Attempt::Without(i, why)),
    };
    module.finalize_definitions().map_err(|e| e.to_string())?;
    for (id, (size, fmaps)) in &maps {
        machine::native::register_stack_maps(module.get_finalized_function(*id) as u64, *size, fmaps.clone());
    }

    let mut entries = Vec::new();
    let mut entry_funcs = Vec::new();
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
        let ret_words = if body.mir.ret_hi_ty.is_some() { 2 } else { 1 };
        let reads = plan.used_params.get(&i).copied().unwrap_or(body.mir.params.len()) as u16;
        let generator = is_generator(&body.mir);
        let cleanup = body.mir.cleanup.is_some();
        let native = NativeEntry { pc, arity: body.mir.params.len() as u16, suspends: plan.runtime_reach.contains(&i), reads, ret_words, generator, cleanup, entry, data: 0 };
        entries.push(native);
        entry_funcs.push(image::Entry { func: ids.entry[&i], entry: native });
    }
    // A program with refused bodies does not run on native code alone.
    let image = recorder.filter(|_| refused.is_empty()).and_then(|recorder| {
        image::encode(image::Parts { module: &module, reserve, recorder, maps: &maps, entries: &entry_funcs, sites: &sites, compiled: &compiled })
    });
    // The code must outlive every `Runtime` that runs it.
    std::mem::forget(module);
    Ok(Attempt::Done(NativeProgram { entries, compiled, refused, sites, image }))
}

/// Address space to reserve for the code and data of `plan`'s bodies: far
/// more than they take (it is committed as used), within a 32-bit offset.
fn jit_reserve(bodies: &[NativeBody], plan: &Plan) -> usize {
    const BYTES_PER_OP: usize = 4096;
    let ops: usize = plan.compile.iter().map(|&i| bodies[i].mir.blocks.iter().map(|b| b.insts.len() + 1).sum::<usize>()).sum();
    (ops.saturating_mul(BYTES_PER_OP)).clamp(64 << 20, 1 << 30)
}

// ---------------------------------------------------------------------------
// Which bodies compile

struct Plan {
    /// Indices into `bodies` that compile.
    compile: Vec<usize>,
    by_label: HashMap<Label, usize>,
    refused: Vec<(String, String)>,
    /// Compiled bodies that may move an array's storage (a push, a host
    /// call, or a call to such a body): their callers re-read
    /// the arrays they view after the call.
    moves: HashSet<usize>,
    /// Compiled bodies in which a collection may run (they allocate,
    /// or call such a body): they have safepoints, and
    /// their callers root the heap words they hold across the call.
    may_gc: HashSet<usize>,
    /// Entry PCs of the bodies by label, for function values.
    pcs: HashMap<Label, u32>,
    /// Bodies that may reach the runtime ([`reaches_runtime`], or a call of
    /// such a body): calls of them link a `NativeCtx::calls` record.
    runtime_reach: HashSet<usize>,
    /// How many leading parameters each compiled body reads
    /// ([`used_params`]): a call may pass fewer words, as a call's
    /// arity may differ from the body's MIR parameters.
    used_params: HashMap<usize, usize>,
    /// Bodies that may be a type's `drop` (a call claims its finalizer).
    drops: HashSet<usize>,
    /// The blocks of each body that end in a tail call ([`tail_sites`]).
    tail_sites: HashMap<usize, HashSet<BlockId>>,
    /// Bodies on the `tail` calling convention: the ones with tail sites
    /// and what they call there. It saves no registers across a call, so
    /// other bodies stay on the platform's.
    tail_conv: HashSet<usize>,
}

/// What [`define_all`] made: the declared ids, the bodies defined, the
/// panic sites their code names, and the stack maps of each function.
type Defined = (Ids, Vec<usize>, Vec<NativeSite>, Vec<(FuncId, FuncMaps)>);

/// Declare and define every body `plan` compiles (its implementation,
/// cleanup pad and driver entry) in `module`. `Ok(Err((i, why)))` when body
/// `i` does not lower: the caller tries again without it.
#[allow(clippy::type_complexity)]
fn define_all<M: Module>(
    module: &mut M,
    bodies: &[NativeBody],
    entry_pcs: &HashMap<String, u32>,
    plan: &Plan,
    mut recorder: Option<&mut image::Recorder>,
) -> Result<Result<Defined, (usize, String)>, String> {
    let ids = declare(module, bodies, plan)?;
    define_trampolines(module, &ids.tramps, recorder.as_deref_mut())?;
    let fcfg = module.isa().frontend_config();
    let mut fbc = FunctionBuilderContext::new();
    let mut ctx = module.make_context();
    let mut defined = Vec::new();
    let mut sites = Vec::new();
    let mut maps = Vec::new();
    for &i in &plan.compile {
        let body = &bodies[i];
        ctx.func.signature = impl_signature(&*module, &body.mir, plan.tail_conv.contains(&i));
        let lowered = {
            let entry_pc = entry_pcs.get(&body.name).copied();
            let mut lower = Lower::new(module, &mut ctx.func, &mut fbc, &ids, plan, &mut sites, entry_pc);
            lower.body_index = Some(i);
            let r = lower.body(&body.mir);
            if r.is_ok() {
                lower.b.finalize(fcfg);
            }
            r
        };
        if let Err(why) = lowered {
            return Ok(Err((i, why)));
        }
        if let Err(e) = module.define_function(ids.imp[&i], &mut ctx) {
            return Ok(Err((i, format!("cranelift: {e:?}"))));
        }
        maps.push((ids.imp[&i], stack_maps(&ctx)));
        if let Some(r) = recorder.as_deref_mut() {
            r.add(ids.imp[&i], &ctx);
        }
        module.clear_context(&mut ctx);

        if let (Some(pad), Some(&pad_id)) = (body.mir.cleanup.as_deref(), ids.pad.get(&i)) {
            ctx.func.signature = entry_signature(&*module);
            let lowered = {
                // Its sites are its body's: a panic in a `defer` names it.
                let entry_pc = entry_pcs.get(&body.name).copied();
                let mut lower = Lower::new(module, &mut ctx.func, &mut fbc, &ids, plan, &mut sites, entry_pc);
                lower.is_pad = true;
                let r = lower.body(pad);
                if r.is_ok() {
                    lower.b.finalize(fcfg);
                }
                r
            };
            if let Err(why) = lowered {
                return Ok(Err((i, format!("cleanup pad: {why}"))));
            }
            if let Err(e) = module.define_function(pad_id, &mut ctx) {
                return Ok(Err((i, format!("cranelift pad: {e:?}"))));
            }
            maps.push((pad_id, stack_maps(&ctx)));
            if let Some(r) = recorder.as_deref_mut() {
                r.add(pad_id, &ctx);
            }
            module.clear_context(&mut ctx);
        }

        ctx.func.signature = entry_signature(&*module);
        build_entry(module, &mut ctx.func, &mut fbc, ids.imp[&i], &body.mir);
        module
            .define_function(ids.entry[&i], &mut ctx)
            .map_err(|e| format!("cranelift entry of `{}`: {e:?}", body.name))?;
        if let Some(r) = recorder.as_deref_mut() {
            r.add(ids.entry[&i], &ctx);
        }
        module.clear_context(&mut ctx);
        defined.push(i);
    }
    Ok(Ok((ids, defined, sites, maps)))
}

/// A compiled function's code size and stack maps: per call that holds
/// heap words across it, the return address's offset in the function and
/// the stack-pointer offsets of the slots that hold them.
pub(crate) type FuncMaps = (u32, Vec<(u32, Vec<u32>)>);

fn stack_maps(ctx: &cranelift_codegen::Context) -> FuncMaps {
    let Some(code) = ctx.compiled_code() else {
        return (0, Vec::new());
    };
    // Each slot by its distance below the frame pointer: a frame's stack
    // pointer at a call is not always where its callee's frame starts, as a
    // `tail` body moves its return address to make room for stack arguments.
    let maps = code.buffer.user_stack_maps().iter().map(|(ra, span, map)| (*ra, map.entries().map(|(_, off)| span - off).collect())).collect();
    (code.code_info().total_size, maps)
}

/// [`plan`] with `excluded` bodies refused for the given reason.
fn plan_without(bodies: &[NativeBody], excluded: &HashMap<usize, String>, entry_pcs: &HashMap<String, u32>) -> Plan {
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
    // A body that calls one that does not compile does not compile either.
    loop {
        let bad: Vec<usize> =
            ok.iter().filter(|(_, callees)| callees.iter().any(|c| !ok.contains_key(c))).map(|(&i, _)| i).collect();
        if bad.is_empty() {
            break;
        }
        for i in bad {
            ok.remove(&i);
            refused.push((bodies[i].name.clone(), "calls a body native code cannot call".to_string()));
        }
    }
    let mut moves: HashSet<usize> = ok
        .iter()
        .filter(|(i, _)| {
                bodies[**i].mir.blocks.iter().flat_map(|b| &b.insts).any(|inst| {
                    matches!(
                        inst,
                        MirInst::ArrayPush { .. } | MirInst::HostInvoke { .. } | MirInst::CallIndirect { .. } | MirInst::ResumeCoro { .. } | MirInst::FreeEarly { .. } | MirInst::Yield { .. } | MirInst::Ffi { .. } | MirInst::VecLoop { .. }
                    )
                })
        })
        .map(|(&i, _)| i)
        .collect();
    loop {
        let more: Vec<usize> = ok
            .iter()
            .filter(|(i, callees)| !moves.contains(i) && callees.iter().any(|c| moves.contains(c)))
            .map(|(&i, _)| i)
            .collect();
        if more.is_empty() {
            break;
        }
        moves.extend(more);
    }
    let mut may_gc: HashSet<usize> = ok
        .iter()
        .filter(|(i, _)| bodies[**i].mir.blocks.iter().flat_map(|b| &b.insts).any(allocates))
        .map(|(&i, _)| i)
        .collect();
    loop {
        let more: Vec<usize> = ok
            .iter()
            .filter(|(i, callees)| !may_gc.contains(i) && callees.iter().any(|c| may_gc.contains(c)))
            .map(|(&i, _)| i)
            .collect();
        if more.is_empty() {
            break;
        }
        may_gc.extend(more);
    }
    let pcs = by_label
        .iter()
        .filter_map(|(&l, &i)| Some((l, *entry_pcs.get(&bodies[i].name)?)))
        .collect();
    let drops = bodies
        .iter()
        .enumerate()
        .filter(|(_, b)| b.name.ends_with("::drop") && b.mir.params.len() == 1)
        .map(|(i, _)| i)
        .collect();
    // Bodies that may run interpreted code (which may panic and print a
    // backtrace): their calls name themselves in `NativeCtx::calls`.
    let has_drops = bodies.iter().any(|b| b.name.ends_with("::drop") && b.mir.params.len() == 1);
    let mut runtime_reach: HashSet<usize> = ok
        .iter()
        .filter(|(i, _)| bodies[**i].mir.blocks.iter().flat_map(|b| &b.insts).any(|inst| reaches_runtime(inst, has_drops)))
        .map(|(&i, _)| i)
        .collect();
    loop {
        let more: Vec<usize> = ok
            .iter()
            .filter(|(i, callees)| !runtime_reach.contains(i) && callees.iter().any(|c| runtime_reach.contains(c)))
            .map(|(&i, _)| i)
            .collect();
        if more.is_empty() {
            break;
        }
        runtime_reach.extend(more);
    }
    let used_params = compile_used_params(bodies, ok.keys());
    let tail_sites = tail_sites(bodies, &ok, &by_label, &drops);
    let mut tail_conv: HashSet<usize> = tail_sites.keys().copied().collect();
    for (i, blocks) in &tail_sites {
        let f = &bodies[*i].mir;
        tail_conv.extend(f.blocks.iter().filter(|b| blocks.contains(&b.id)).filter_map(|b| match b.insts.last() {
            Some(MirInst::Call { target, .. }) => by_label.get(target).copied(),
            _ => None,
        }));
    }
    let mut compile: Vec<usize> = ok.into_keys().collect();
    compile.sort_unstable();
    Plan { compile, by_label, refused, moves, may_gc, pcs, drops, runtime_reach, used_params, tail_sites, tail_conv }
}

/// An instruction that may run interpreted code: a function value's call, a free
/// (in a program with `drop`s) or collection that may run `drop`s.
fn reaches_runtime(inst: &MirInst, drops: bool) -> bool {
    match inst {
        MirInst::CallIndirect { .. } | MirInst::ResumeCoro { .. } | MirInst::Ffi { .. } => true,
        MirInst::FreeEarly { .. } => drops,
        MirInst::HostInvoke { native_id, .. } => {
            machine::native::direct_host_natives().get(usize::from(*native_id)).is_some_and(|h| h.collect || h.on_machine)
        }
        _ => false,
    }
}

/// [`used_params`] of each of `bodies` at `which`.
fn compile_used_params<'a>(bodies: &[NativeBody], which: impl Iterator<Item = &'a usize>) -> HashMap<usize, usize> {
    which.map(|&i| (i, used_params(&bodies[i].mir))).collect()
}

/// The blocks of each compiled body whose last op takes the frame, as the
/// interpreter's does: a planned tail call in a body with no `defer`s. Here
/// also none that holds a `gc::root` handle until it returns, and only where
/// both bodies return the same words, which `return_call` needs.
fn tail_sites(bodies: &[NativeBody], ok: &HashMap<usize, Vec<usize>>, by_label: &HashMap<Label, usize>, drops: &HashSet<usize>) -> HashMap<usize, HashSet<BlockId>> {
    let returns = |f: &MirFunc| (cl_ty(f.ret_ty.unwrap_or(MirTy::I64)), f.ret_hi_ty.map(cl_ty));
    let mut sites: HashMap<usize, HashSet<BlockId>> = HashMap::new();
    for &i in ok.keys() {
        let f = &bodies[i].mir;
        if !f.homes.is_empty() || f.cleanup.is_some() || !root_handles(f).is_empty() || is_generator(f) {
            continue;
        }
        for b in &f.blocks {
            let Some(inst) = b.insts.last().filter(|inst| compiler::native_mir::is_tail_call_inst(f, b, inst)) else {
                continue;
            };
            let MirInst::Call { target, .. } = inst else { continue };
            let Some(&callee) = by_label.get(target) else { continue };
            if ok.contains_key(&callee) && !drops.contains(&callee) && returns(f) == returns(&bodies[callee].mir) {
                sites.entry(i).or_default().insert(b.id);
            }
        }
    }
    sites
}

/// Whether any instruction or terminator of `f` reads `v`.
fn value_used(f: &MirFunc, v: ValueId) -> bool {
    f.blocks.iter().any(|b| {
        b.insts.iter().any(|i| i.operands().contains(&v))
            || match &b.term {
                Some(Terminator::Br { cond, .. }) => *cond == v,
                Some(Terminator::JumpIfMatch { scrutinee, payloads, .. }) => *scrutinee == v || payloads.contains(&v),
                Some(Terminator::Return { lo, hi }) => *lo == Some(v) || *hi == Some(v),
                _ => false,
            }
    })
}

/// The callees of `f` when its body is in the numeric tier.
fn eligible(f: &MirFunc, by_label: &HashMap<Label, usize>) -> Result<Vec<usize>, String> {
    eligible_in(f, by_label, false)
}

/// [`eligible`] of a body, or of a cleanup pad (`pad`).
fn eligible_in(f: &MirFunc, by_label: &HashMap<Label, usize>, pad: bool) -> Result<Vec<usize>, String> {
    if let Some(why) = f.emit_refusal {
        return Err(format!("no emission: {why}"));
    }
    if f.homes.len() > 64 {
        return Err("more than 64 defer homes".into());
    }
    if let Some(t) = f.homes.iter().find(|t| !numeric(**t)) {
        return Err(format!("defer home of {t:?}"));
    }
    for &p in &f.params {
        if !numeric(ty_of(f, p)) {
            return Err(format!("parameter type {:?}", ty_of(f, p)));
        }
    }
    // One word, two (a two-slot return: `lo`, then `hi`), or none (the
    // runtime returns a `0` word).
    match (f.ret_ty, f.ret_hi_ty) {
        (None, None) => {}
        (Some(t), None) if numeric(t) => {}
        (Some(t), Some(h)) if numeric(t) && numeric(h) => {}
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
                        if *ty == MirTy::Bool && !op.requires_int() {
                            return Err("bool arithmetic".into());
                        }
                    }
                    // Heap words compare as `i64` words, as `DenseCmp` does.
                    MirInst::Cmp { dest, ty, lhs, rhs, .. } => {
                        if !scalar(*ty) && !heap_word(*ty) {
                            return Err(format!("compare of {ty:?}"));
                        }
                        need_scalar(dest)?;
                        need(lhs)?;
                        need(rhs)?;
                    }
                    // Native code roots every heap word it holds across a
                    // call that may collect: nothing to keep.
                    MirInst::KeepAlive { dest, value } => {
                        need(dest)?;
                        need(value)?;
                    }
                    MirInst::Probe { dest, .. } => need(dest)?,
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
                            MirAllocKind::DropEnum { .. } => {}
                            MirAllocKind::Object { .. } if elems.is_empty() => {}
                            // A function value: its body must have an entry
                            // id, so it counts as a callee.
                            MirAllocKind::Fn { target, .. }
                            | MirAllocKind::PolyFn { target }
                            | MirAllocKind::PolyFnCapture { target, .. }
                            | MirAllocKind::Coro { target } => {
                                let Some(&callee) = by_label.get(target) else {
                                    return Err("function value of an unknown body".into());
                                };
                                callees.push(callee);
                            }
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
                        if let Some(h) = dest_hi {
                            need(h)?;
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
                    MirInst::String { dest, .. } => need(dest)?,
                    // A code pointer (a dictionary method's entry id).
                    MirInst::CodePtr { dest, target } => {
                        need(dest)?;
                        let Some(&callee) = by_label.get(target) else {
                            return Err("code pointer of an unknown body".into());
                        };
                        callees.push(callee);
                    }
                    MirInst::HomeLoad { dest, home } => {
                        need(dest)?;
                        if usize::from(*home) >= f.homes.len() {
                            return Err("home out of range".into());
                        }
                    }
                    MirInst::HomeStore { dest, home, value } => {
                        need(dest)?;
                        need(value)?;
                        if usize::from(*home) >= f.homes.len() {
                            return Err("home out of range".into());
                        }
                    }
                    MirInst::LoadStatic { dest, .. } => need(dest)?,
                    MirInst::StoreStatic { dest, value, .. } => {
                        need(dest)?;
                        need(value)?;
                    }
                    MirInst::FreeEarly { dest, value, .. } => {
                        need(dest)?;
                        need(value)?;
                    }
                    MirInst::Print { dest, src } => {
                        need(dest)?;
                        need(src)?;
                    }
                    MirInst::CallIndirect { dest, callee, args, .. } => {
                        need(dest)?;
                        need(callee)?;
                        for a in args {
                            need(a)?;
                        }
                    }
                    MirInst::ResumeCoro { dest, handle, send } => {
                        need(dest)?;
                        need(handle)?;
                        if let Some(s) = send {
                            need(s)?;
                        }
                    }
                    MirInst::DoneCoro { dest, handle } => {
                        need_scalar(dest)?;
                        need(handle)?;
                    }
                    // Through its stub on the runtime (`coil_native_ffi`).
                    MirInst::Ffi { dest, args, .. } => {
                        need(dest)?;
                        for a in args {
                            need(a)?;
                        }
                        if args.len() > 16 {
                            return Err("FFI op of more than 16 words".into());
                        }
                    }
                    // A generator body: it runs on a fiber of its own and a
                    // yield suspends it (`machine::native_gen`).
                    MirInst::Yield { dest, value, from, .. } => {
                        let _ = from;
                        if f.ret_hi_ty.is_some() {
                            return Err("generator returning two words".into());
                        }
                        need(dest)?;
                        need(value)?;
                    }
                    MirInst::Panic { msg, .. } => need(msg)?,
                    // The runtime's lane code (`VScan` / `VLoop`) over a
                    // frame of the values' words (`coil_native_vector`).
                    MirInst::ByteScan { .. } | MirInst::VecLoop { .. } => {
                        for v in inst.dests().chain(inst.operands()) {
                            need(&v)?;
                        }
                        if compiler::native_mir::vector_op(inst).is_none() {
                            return Err("vector op the lane encoding cannot hold".into());
                        }
                    }
                    // A cleanup pad's end (lowered to nothing).
                    MirInst::HostInvoke { dest, native_id, .. } if pad && *native_id == common::UNWIND_RESUME_ID => {
                        need(dest)?;
                    }
                    MirInst::HostInvoke { dest, native_id, args, .. } => {
                        need(dest)?;
                        for a in args {
                            need(a)?;
                        }
                        match machine::native::direct_host_natives().get(usize::from(*native_id)) {
                            Some(h) if (h.func.is_some() || h.collect) && args.len() <= 16 => {}
                            Some(h) if h.on_machine && args.len() < 16 => {}
                            Some(h) => return Err(format!("host call `{}` has no native path", h.name)),
                            None => return Err(format!("host call {native_id} outside the standard table")),
                        }
                    }
                    MirInst::Format { dest, fmt, args } => {
                        need(dest)?;
                        need(fmt)?;
                        for a in args {
                            need(a)?;
                        }
                    }
                    MirInst::Stringify { dest, src, .. } => {
                        need(dest)?;
                        need(src)?;
                    }
                    MirInst::StrEq { dest, lhs, rhs, .. } => {
                        need_scalar(dest)?;
                        need(lhs)?;
                        need(rhs)?;
                    }
                    // Native code keeps no frame words for the collector to
                    // scan, so there is nothing to zero.
                    MirInst::ClearWord { dest, .. } => need(dest)?,
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
            Some(Terminator::Return { lo, hi }) => {
                if lo.is_some() != f.ret_ty.is_some() || hi.is_some() != f.ret_hi_ty.is_some() {
                    return Err("return width".into());
                }
                for v in lo.iter().chain(hi) {
                    need(v)?;
                }
            }
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
            None => return Err("block without terminator".into()),
        }
    }
    // A body with `defer`s compiles with its cleanup pad, or not at all.
    if let Some(cleanup) = &f.cleanup {
        if pad || cleanup.cleanup.is_some() {
            return Err("defer pad shape".into());
        }
        callees.extend(eligible_in(cleanup, by_label, true).map_err(|why| format!("defer pad: {why}"))?);
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
    /// The trampolines to helpers that may collect, with the helper and
    /// their signature ([`trampoline`]).
    tramps: Vec<(FuncId, FuncId, Signature)>,
    /// Native-convention body: `(ctx, params..) -> ret`.
    imp: HashMap<usize, FuncId>,
    /// driver entry: `(ctx, *const u64) -> u64`.
    entry: HashMap<usize, FuncId>,
    /// Cleanup pad of a body with `defer`s: `(ctx, *const u64 homes) -> u64`.
    pad: HashMap<usize, FuncId>,
    defer: FuncId,
    claim: FuncId,
    fmod: FuncId,
    powf: FuncId,
    ipow: FuncId,
    /// Heap reads (`machine::native::coil_native_*`).
    index: FuncId,
    len: FuncId,
    field: FuncId,
    store_index: FuncId,
    set_field: FuncId,
    view: FuncId,
    /// Copy-on-write (these allocate, never collect).
    cow: FuncId,
    cow_field: FuncId,
    /// Allocation (`coil_native_alloc`, `coil_native_array_push`).
    alloc: FuncId,
    array_push: FuncId,
    probe: FuncId,
    /// Boxed enum matches.
    enum_tag: FuncId,
    enum_payload: FuncId,
    /// Program strings (`STRING`).
    string: FuncId,
    /// Content / structural `==` (`StrEq`).
    eq: FuncId,
    /// `STRINGIFY`.
    stringify: FuncId,
    /// `FORMAT`.
    format: FuncId,
    /// `FreeEarly` (queued for the runtime).
    free_early: FuncId,
    /// Bails and native panics (`coil_native_unwind`).
    unwind: FuncId,
    /// `PRINT`.
    print: FuncId,
    /// Direct host natives.
    host: FuncId,
    ffi: FuncId,
    /// `ByteScan` / `VecLoop` (`coil_native_vector`).
    vector: FuncId,
    /// Calls to bodies on the runtime.
    call_value: FuncId,
    resume: FuncId,
    coro_done: FuncId,
    gen_yield: FuncId,
    /// Collections at safepoints.
    collect: FuncId,
    /// Explicit `panic`.
    panic: FuncId,
}

fn cl_ty(t: MirTy) -> Type {
    if t == MirTy::F64 { types::F64 } else { types::I64 }
}

fn impl_signature(module: &impl Module, f: &MirFunc, tail: bool) -> Signature {
    let mut sig = module.make_signature();
    if tail {
        sig.call_conv = CallConv::Tail;
    }
    sig.params.push(AbiParam::new(types::I64));
    for &p in &f.params {
        sig.params.push(AbiParam::new(cl_ty(ty_of(f, p))));
    }
    sig.returns.push(AbiParam::new(cl_ty(f.ret_ty.unwrap_or(MirTy::I64))));
    if let Some(h) = f.ret_hi_ty {
        sig.returns.push(AbiParam::new(cl_ty(h)));
    }
    sig
}

fn entry_signature(module: &impl Module) -> Signature {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    sig
}

/// Declare the helper `symbol` and a local trampoline to it with the same
/// signature: native code calls a helper that may collect through its
/// trampoline, which records its frame pointer in the context so the
/// collector can walk the native frames ([`define_trampolines`]).
fn trampoline(module: &mut impl Module, symbol: &str, sig: &Signature, tramps: &mut Vec<(FuncId, FuncId, Signature)>) -> Result<FuncId, String> {
    let e = |e: cranelift_module::ModuleError| e.to_string();
    let helper = module.declare_function(symbol, Linkage::Import, sig).map_err(e)?;
    let tramp = module.declare_function(&format!("{symbol}_t"), Linkage::Local, sig).map_err(e)?;
    tramps.push((tramp, helper, sig.clone()));
    Ok(tramp)
}

/// Define [`trampoline`]s: `ctx.native_fp = fp; return helper(args...)`.
fn define_trampolines<M: Module>(module: &mut M, tramps: &[(FuncId, FuncId, Signature)], mut recorder: Option<&mut image::Recorder>) -> Result<(), String> {
    let fcfg = module.isa().frontend_config();
    let mut fbc = FunctionBuilderContext::new();
    let mut ctx = module.make_context();
    for (tramp, helper, sig) in tramps {
        ctx.func.signature = sig.clone();
        let callee = module.declare_func_in_func(*helper, &mut ctx.func);
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
        let blk = b.create_block();
        b.append_block_params_for_function_params(blk);
        b.switch_to_block(blk);
        let args = b.block_params(blk).to_vec();
        let fp = b.ins().get_frame_pointer(types::I64);
        b.ins().store(MemFlagsData::trusted(), fp, args[0], machine::native::NATIVE_CTX_NATIVE_FP);
        let call = b.ins().call(callee, &args);
        let rets = b.inst_results(call).to_vec();
        b.ins().return_(&rets);
        b.seal_all_blocks();
        b.finalize(fcfg);
        module.define_function(*tramp, &mut ctx).map_err(|e| format!("cranelift trampoline: {e:?}"))?;
        if let Some(r) = recorder.as_deref_mut() {
            r.add(*tramp, &ctx);
        }
        module.clear_context(&mut ctx);
    }
    Ok(())
}

fn declare(module: &mut impl Module, bodies: &[NativeBody], plan: &Plan) -> Result<Ids, String> {
    let compile = &plan.compile;
    let e = |e: cranelift_module::ModuleError| e.to_string();
    let mut tramps = Vec::new();
    let mut imp = HashMap::new();
    let mut entry = HashMap::new();
    let mut pad = HashMap::new();
    for &i in compile {
        let sig = impl_signature(module, &bodies[i].mir, plan.tail_conv.contains(&i));
        imp.insert(i, module.declare_function(&format!("coil_impl_{i}"), Linkage::Local, &sig).map_err(e)?);
        let esig = entry_signature(module);
        entry.insert(i, module.declare_function(&format!("coil_entry_{i}"), Linkage::Local, &esig).map_err(e)?);
        if bodies[i].mir.cleanup.is_some() {
            pad.insert(i, module.declare_function(&format!("coil_pad_{i}"), Linkage::Local, &esig).map_err(e)?);
        }
    }
    let mut f2 = module.make_signature();
    f2.params.push(AbiParam::new(types::F64));
    f2.params.push(AbiParam::new(types::F64));
    f2.returns.push(AbiParam::new(types::F64));
    let fmod = module.declare_function("coil_native_fmod", Linkage::Import, &f2).map_err(e)?;
    let powf = module.declare_function("coil_native_powf", Linkage::Import, &f2).map_err(e)?;
    let mut i3 = module.make_signature();
    i3.params.extend((0..3).map(|_| AbiParam::new(types::I64)));
    i3.returns.push(AbiParam::new(types::I64));
    let ipow = module.declare_function("coil_native_ipow", Linkage::Import, &i3).map_err(e)?;
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
    let cow = module.declare_function("coil_native_cow", Linkage::Import, &s3).map_err(e)?;
    let cow_field = module.declare_function("coil_native_cow_field", Linkage::Import, &s3).map_err(e)?;
    let alloc = module.declare_function("coil_native_alloc", Linkage::Import, &s5).map_err(e)?;
    let array_push = module.declare_function("coil_native_array_push", Linkage::Import, &s3).map_err(e)?;
    let probe = module.declare_function("coil_native_probe", Linkage::Import, &s2).map_err(e)?;
    let free_early = trampoline(module, "coil_native_free_early", &s3, &mut tramps)?;
    let format = module.declare_function("coil_native_format", Linkage::Import, &s4).map_err(e)?;
    let stringify = module.declare_function("coil_native_stringify", Linkage::Import, &s3).map_err(e)?;
    let eq = module.declare_function("coil_native_eq", Linkage::Import, &s3).map_err(e)?;
    let string = module.declare_function("coil_native_string", Linkage::Import, &s2).map_err(e)?;
    let enum_tag = module.declare_function("coil_native_enum_tag", Linkage::Import, &s1).map_err(e)?;
    let enum_payload = module.declare_function("coil_native_enum_payload", Linkage::Import, &s3).map_err(e)?;
    let mut s3v = module.make_signature();
    s3v.params.extend((0..3).map(|_| AbiParam::new(types::I64)));
    let unwind = module.declare_function("coil_native_unwind", Linkage::Import, &s3v).map_err(e)?;
    let print = module.declare_function("coil_native_print", Linkage::Import, &s2).map_err(e)?;
    let host = trampoline(module, "coil_native_host", &s5, &mut tramps)?;
    let ffi = trampoline(module, "coil_native_ffi", &s5, &mut tramps)?;
    let mut s9 = module.make_signature();
    s9.params.extend((0..9).map(|_| AbiParam::new(types::I64)));
    s9.returns.push(AbiParam::new(types::I64));
    let vector = module.declare_function("coil_native_vector", Linkage::Import, &s9).map_err(e)?;
    let call_value = trampoline(module, "coil_native_call_value", &s5, &mut tramps)?;
    let resume = trampoline(module, "coil_native_resume", &s4, &mut tramps)?;
    let coro_done = module.declare_function("coil_native_coro_done", Linkage::Import, &s2).map_err(e)?;
    let gen_yield = trampoline(module, "coil_native_yield", &s2, &mut tramps)?;
    let mut s5v = module.make_signature();
    s5v.params.extend((0..5).map(|_| AbiParam::new(types::I64)));
    let defer = module.declare_function("coil_native_defer", Linkage::Import, &s5v).map_err(e)?;
    let claim = module.declare_function("coil_native_claim", Linkage::Import, &s3).map_err(e)?;
    let mut s1v2 = module.make_signature();
    s1v2.params.push(AbiParam::new(types::I64));
    let collect = trampoline(module, "coil_native_collect", &s1v2, &mut tramps)?;
    let panic = module.declare_function("coil_native_panic", Linkage::Import, &s2).map_err(e)?;
    Ok(Ids {
        tramps,
        imp,
        entry,
        pad,
        defer,
        claim,
        fmod,
        powf,
        ipow,
        index,
        len,
        field,
        store_index,
        set_field,
        view,
        cow,
        cow_field,
        alloc,
        array_push,
        probe,
        enum_tag,
        enum_payload,
        string,
        eq,
        stringify,
        format,
        free_early,
        unwind,
        print,
        host,
        ffi,
        vector,
        call_value,
        resume,
        coro_done,
        gen_yield,
        collect,
        panic,
    })
}

// ---------------------------------------------------------------------------
// The runtime entry wrapper

fn build_entry(
    module: &mut impl Module,
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
            // The runtime reads a bool word's low byte: `== 1` is true.
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
    let results = b.inst_results(call).to_vec();
    let word = |b: &mut FunctionBuilder, x: cranelift_codegen::ir::Value| {
        if b.func.dfg.value_type(x) == types::F64 { b.ins().bitcast(types::I64, MemFlagsData::new(), x) } else { x }
    };
    let out = word(&mut b, results[0]);
    if let Some(&hi) = results.get(1) {
        let hi = word(&mut b, hi);
        b.ins().store(MemFlagsData::trusted(), hi, ctx, NATIVE_CTX_RET_HI);
    }
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
    ipow: FuncRef,
    index: FuncRef,
    len: FuncRef,
    field: FuncRef,
    store_index: FuncRef,
    set_field: FuncRef,
    view: FuncRef,
    cow: FuncRef,
    cow_field: FuncRef,
    alloc: FuncRef,
    array_push: FuncRef,
    probe: FuncRef,
    enum_tag: FuncRef,
    enum_payload: FuncRef,
    string: FuncRef,
    eq: FuncRef,
    stringify: FuncRef,
    format: FuncRef,
    free_early: FuncRef,
    unwind: FuncRef,
    print: FuncRef,
    host: FuncRef,
    ffi: FuncRef,
    vector: FuncRef,
    call_value: FuncRef,
    resume: FuncRef,
    coro_done: FuncRef,
    gen_yield: FuncRef,
    collect: FuncRef,
    panic: FuncRef,
    /// This body's index into the plan's bodies.
    body_index: Option<usize>,
    /// Lowering a cleanup pad: its homes are behind its second parameter.
    is_pad: bool,
    /// Where the homes live: this frame's slot (`[next, n, words...]` is
    /// not needed: they are copied out at a panic), or the pad's pointer.
    homes_slot: Option<cranelift_codegen::ir::StackSlot>,
    homes_ptr: Option<cranelift_codegen::ir::Value>,
    /// Homes holding heap words, rooted with the body's heap values.
    heap_homes: Vec<u16>,
    /// An opaque body's `int` homes, which may hold objects: rooted
    /// ambiguously.
    amb_homes: Vec<u16>,
    /// The [`Self::push_call`] record of this frame.
    calls_slot: Option<cranelift_codegen::ir::StackSlot>,
    /// An opaque body's `int` words that may be objects: rooted in an
    /// ambiguous record (pinned, never rewritten).
    opaque: Vec<ValueId>,
    /// Words of a non-heap type that hold an object for sure
    /// ([`pointer_words`]): rooted precisely, as heap words are.
    ptr_words: HashSet<ValueId>,
    /// `gc::root` handles the body makes: kept alive until it returns
    /// ([`Self::keep_roots`]), where other words die at their last use.
    root_handles: Vec<ValueId>,
    /// Where [`Self::keep_roots`] stores them (made on first use).
    keep_slot: Option<cranelift_codegen::ir::StackSlot>,
    /// Words of pairs that may carry an object ([`pair_words`]): rooted
    /// ambiguously.
    pair_words: Vec<ValueId>,
    /// The heap words the op being lowered hands the runtime. The runtime
    /// holds its own copy of them, and a word past its last use is in no
    /// stack map: rooted ambiguously, so the object stays where it is. A
    /// direct call hands its words to a frame that roots them.
    op_words: Vec<ValueId>,
    defer: FuncRef,
    claim: FuncRef,
    /// This body's cleanup pad.
    pad_fn: Option<FuncRef>,
    pad_refs: HashMap<usize, FuncRef>,
    /// Panic sites of every body so far; this body's ops name theirs by
    /// index ([`Self::site`]).
    sites: &'a mut Vec<NativeSite>,
    /// This body's entry id, which its sites name (`None`: no sites).
    entry_pc: Option<u32>,
    /// The location of the op being lowered, and its site once a bail
    /// path named it.
    cur_loc: Option<common::DebugLoc>,
    cur_site: Option<i64>,
    /// Arrays this body indexes: `(ok, pointer, length)` of their element
    /// words in place, set where the array value is defined (see
    /// `machine::native::coil_native_view`).
    views: HashMap<ValueId, (Variable, Variable, Variable)>,
    /// The MIR values to root at the op being lowered (`None`: all of
    /// them): those live there and the words locals hold ([`held_words`]).
    /// [`Self::push_roots`] records only these, so a dead temporary is
    /// not kept alive by this frame.
    live: Option<HashSet<ValueId>>,
    /// Array values this body stores into.
    stored: HashSet<ValueId>,
    /// `GcBarrier` dest -> the object it names ([`barrier_pairs`]).
    barriers: HashMap<ValueId, ValueId>,
    /// Two words `coil_native_view` writes to.
    view_slot: cranelift_codegen::ir::StackSlot,
    by_label: &'a HashMap<Label, usize>,
    plan: &'a Plan,
    vars: Vec<Option<Variable>>,
    blocks: HashMap<BlockId, Block>,
    ctx: cranelift_codegen::ir::Value,
    bail: Block,
    ret: Type,
    /// The second result's type of a two-word return.
    ret_hi: Option<Type>,
}

impl<'a> Lower<'a> {
    fn new(
        module: &mut impl Module,
        func: &'a mut cranelift_codegen::ir::Function,
        fbc: &'a mut FunctionBuilderContext,
        ids: &Ids,
        plan: &'a Plan,
        sites: &'a mut Vec<NativeSite>,
        entry_pc: Option<u32>,
    ) -> Self {
        let by_label = &plan.by_label;
        let mut module_refs = HashMap::new();
        for (&i, &id) in &ids.imp {
            module_refs.insert(i, module.declare_func_in_func(id, func));
        }
        let fmod = module.declare_func_in_func(ids.fmod, func);
        let powf = module.declare_func_in_func(ids.powf, func);
        let ipow = module.declare_func_in_func(ids.ipow, func);
        let index = module.declare_func_in_func(ids.index, func);
        let len = module.declare_func_in_func(ids.len, func);
        let field = module.declare_func_in_func(ids.field, func);
        let store_index = module.declare_func_in_func(ids.store_index, func);
        let set_field = module.declare_func_in_func(ids.set_field, func);
        let view = module.declare_func_in_func(ids.view, func);
        let cow = module.declare_func_in_func(ids.cow, func);
        let cow_field = module.declare_func_in_func(ids.cow_field, func);
        let alloc = module.declare_func_in_func(ids.alloc, func);
        let array_push = module.declare_func_in_func(ids.array_push, func);
        let probe = module.declare_func_in_func(ids.probe, func);
        let enum_tag = module.declare_func_in_func(ids.enum_tag, func);
        let enum_payload = module.declare_func_in_func(ids.enum_payload, func);
        let string = module.declare_func_in_func(ids.string, func);
        let eq = module.declare_func_in_func(ids.eq, func);
        let stringify = module.declare_func_in_func(ids.stringify, func);
        let format = module.declare_func_in_func(ids.format, func);
        let free_early = module.declare_func_in_func(ids.free_early, func);
        let unwind = module.declare_func_in_func(ids.unwind, func);
        let print = module.declare_func_in_func(ids.print, func);
        let host = module.declare_func_in_func(ids.host, func);
        let ffi = module.declare_func_in_func(ids.ffi, func);
        let vector = module.declare_func_in_func(ids.vector, func);
        let call_value = module.declare_func_in_func(ids.call_value, func);
        let resume = module.declare_func_in_func(ids.resume, func);
        let coro_done = module.declare_func_in_func(ids.coro_done, func);
        let gen_yield = module.declare_func_in_func(ids.gen_yield, func);
        let collect = module.declare_func_in_func(ids.collect, func);
        let panic = module.declare_func_in_func(ids.panic, func);
        let defer = module.declare_func_in_func(ids.defer, func);
        let claim = module.declare_func_in_func(ids.claim, func);
        let pad_refs = ids.pad.iter().map(|(&i, &id)| (i, module.declare_func_in_func(id, func))).collect();
        let view_slot = func.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
            16,
            3,
        ));
        let ret = func.signature.returns[0].value_type;
        let ret_hi = func.signature.returns.get(1).map(|r| r.value_type);
        let mut b = FunctionBuilder::new(func, fbc);
        // Params: the `TRAP_*` code and the site.
        let bail = b.create_block();
        b.append_block_param(bail, types::I64);
        b.append_block_param(bail, types::I64);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        let ctx = b.block_params(entry)[0];
        b.switch_to_block(entry);
        Self {
            b,
            module_refs,
            fmod,
            powf,
            ipow,
            index,
            len,
            field,
            store_index,
            set_field,
            view,
            cow,
            cow_field,
            alloc,
            array_push,
            probe,
            enum_tag,
            enum_payload,
            string,
            eq,
            stringify,
            format,
            free_early,
            unwind,
            print,
            host,
            ffi,
            vector,
            call_value,
        resume,
        coro_done,
        gen_yield,
            collect,
            panic,
            body_index: None,
            is_pad: false,
            homes_slot: None,
            homes_ptr: None,
            heap_homes: Vec::new(),
            amb_homes: Vec::new(),
            opaque: Vec::new(),
            ptr_words: HashSet::new(),
            root_handles: Vec::new(),
            keep_slot: None,
            pair_words: Vec::new(),
            op_words: Vec::new(),
            calls_slot: None,
            defer,
            claim,
            pad_fn: None,
            pad_refs,
            sites,
            entry_pc,
            cur_loc: None,
            cur_site: None,
            views: HashMap::new(),
            live: None,
            stored: HashSet::new(),
            barriers: HashMap::new(),
            view_slot,
            by_label,
            plan,
            vars: Vec::new(),
            blocks: HashMap::new(),
            ctx,
            bail,
            ret,
            ret_hi,
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
        // The collector finds and rewrites heap words in native frames by
        // their stack maps, live at last use.
        if heap_word(ty_of(f, v)) || self.ptr_words.contains(&v) {
            self.b.declare_var_needs_stack_map(var);
        }
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
    /// a caller's `int`-typed value, as the runtime's untyped words allow).
    fn coerce(&mut self, x: cranelift_codegen::ir::Value, want: Type) -> cranelift_codegen::ir::Value {
        if self.b.func.dfg.value_type(x) == want {
            x
        } else {
            self.b.ins().bitcast(want, MemFlagsData::new(), x)
        }
    }

    /// Before a call in which a collection may run: link a record of the
    /// heap words the stack maps cannot describe into the context's root
    /// chain (see `machine::native::NativeCtx::roots`): the heap homes,
    /// which live in memory, and the ambiguous words, which are pinned.
    /// Heap words in values are the stack maps'
    /// (`declare_var_needs_stack_map`). Returns the chain's previous head,
    /// for [`Self::pop_roots`].
    fn push_roots(&mut self, f: &MirFunc) -> Option<Roots> {
        let homes = if self.homes_slot.is_some() { self.heap_homes.clone() } else { Vec::new() };
        let amb_homes = if self.homes_slot.is_some() { self.amb_homes.clone() } else { Vec::new() };
        let opaque: Vec<ValueId> = self
            .opaque
            .iter()
            .chain(&self.pair_words)
            .chain(&self.op_words)
            .copied()
            .filter(|v| self.live.as_ref().is_none_or(|l| l.contains(v)))
            .collect();
        if homes.is_empty() && opaque.is_empty() && amb_homes.is_empty() {
            return None;
        }
        let t = MemFlagsData::trusted();
        let prev = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_ROOTS);
        // The ambiguous record links behind the precise one; nothing
        // rewrites it, so nothing reloads from it.
        let mut next = prev;
        if !opaque.is_empty() || !amb_homes.is_empty() {
            let amb = self.record(f, next, &opaque, &amb_homes, ROOTS_AMBIGUOUS);
            next = self.b.ins().stack_addr(types::I64, amb, 0);
        }
        let slot = self.record(f, next, &[], &homes, 0);
        let at = self.b.ins().stack_addr(types::I64, slot, 0);
        self.b.ins().store(t, at, self.ctx, NATIVE_CTX_ROOTS);
        let epoch = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_GC_EPOCH);
        Some(Roots { prev, slot, homes, epoch })
    }

    /// A root record `[next, n | flags, words...]` of `vals` and then of
    /// the words in `homes`, in a new slot.
    fn record(&mut self, f: &MirFunc, next: cranelift_codegen::ir::Value, vals: &[ValueId], homes: &[u16], flags: u64) -> cranelift_codegen::ir::StackSlot {
        let total = vals.len() + homes.len();
        let slot = self.b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
            ((total + 2) * 8) as u32,
            3,
        ));
        self.b.ins().stack_store(types::I64, next, slot, 0);
        let n = self.b.ins().iconst(types::I64, (total as u64 | flags) as i64);
        self.b.ins().stack_store(types::I64, n, slot, 8);
        for (i, v) in vals.iter().enumerate() {
            let x = self.get(f, *v);
            let x = self.coerce(x, types::I64);
            self.b.ins().stack_store(types::I64, x, slot, ((i + 2) * 8) as i32);
        }
        let t = MemFlagsData::trusted();
        for (k, &h) in homes.iter().enumerate() {
            let (base, off) = self.home_at(h).expect("homes slot");
            let x = self.b.ins().load(types::I64, t, base, off);
            self.b.ins().stack_store(types::I64, x, slot, ((vals.len() + k + 2) * 8) as i32);
        }
        slot
    }

    /// Unlink [`Self::push_roots`]'s record. When a collection may have run
    /// meanwhile, write the heap homes back from it (a minor collection
    /// moves young objects and rewrites the record) and re-read every
    /// array view.
    fn pop_roots(&mut self, roots: Option<Roots>) {
        let Some(Roots { prev, slot, homes, epoch }) = roots else {
            return;
        };
        let t = MemFlagsData::trusted();
        self.b.ins().store(t, prev, self.ctx, NATIVE_CTX_ROOTS);
        let now = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_GC_EPOCH);
        let moved = self.b.ins().icmp(IntCC::NotEqual, now, epoch);
        let cold = self.b.create_block();
        let go = self.b.create_block();
        self.b.ins().brif(moved, cold, &[], go, &[]);
        self.b.set_cold_block(cold);
        self.b.switch_to_block(cold);
        for (k, &h) in homes.iter().enumerate() {
            let x = self.b.ins().stack_load(types::I64, types::I64, slot, ((k + 2) * 8) as i32);
            let (base, off) = self.home_at(h).expect("homes slot");
            self.b.ins().store(t, x, base, off);
        }
        self.drop_views();
        self.b.ins().jump(go, &[]);
        self.b.switch_to_block(go);
    }

    /// A safepoint: when a collection is due, collect with this body's heap
    /// words rooted.
    fn safepoint(&mut self, f: &MirFunc) {
        let due = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_GC_DUE);
        let cold = self.b.create_block();
        let go = self.b.create_block();
        self.b.ins().brif(due, cold, &[], go, &[]);
        self.b.set_cold_block(cold);
        self.b.switch_to_block(cold);
        let roots = self.push_roots(f);
        self.b.ins().call(self.collect, &[self.ctx]);
        self.pop_roots(roots);
        self.b.ins().jump(go, &[]);
        self.b.switch_to_block(go);
    }

    /// After an op that may move an array's storage (a push, a host or runtime
    /// call), when other values may name the same array: every view reads
    /// through the helpers until its value is defined again.
    fn drop_views(&mut self) {
        let views: Vec<_> = self.views.values().map(|v| v.0).collect();
        let zero = self.b.ins().iconst(types::I64, 0);
        for ok in views {
            self.b.def_var(ok, zero);
        }
    }

    /// Branch to the bail block when `cond` is non-zero, stopping at the
    /// current op for reason `code` (a `TRAP_*`); continue in a new block.
    /// A `VecLoop` reduce with no stores as a native loop over its lanes,
    /// one lane at a time, folding left to right as the runtime does. Anything
    /// the runtime's lanes would trap on, or a load outside its array's view,
    /// leaves the loop and runs the whole op on the runtime's lane code from the
    /// start (a pure reduce: nothing it did shows), which gives the runtime's
    /// own result or trap. `false`: not such a reduce (nothing emitted).
    fn native_reduce(&mut self, f: &MirFunc, inst: &MirInst) -> Result<bool, String> {
        use compiler::native_mir::VecLoopKind;
        use common::{dense, simd};
        let MirInst::VecLoop { dest, pos, from, to, kind: VecLoopKind::Reduce { init, ty, fold, term, stores } } = inst else {
            return Ok(false);
        };
        let float = match *ty {
            dense::TY_I64 => false,
            dense::TY_F64 => true,
            _ => return Ok(false),
        };
        let fold_ok = match *fold {
            simd::REDUCE_ADD | simd::REDUCE_MUL => true,
            simd::REDUCE_AND | simd::REDUCE_OR | simd::REDUCE_XOR => !float,
            k => (simd::REDUCE_TAKE..simd::REDUCE_TAKE + 6).contains(&k),
        };
        if !stores.is_empty() || !fold_ok || pos.is_some_and(|p| value_used(f, p)) || !self.lanes_native(term, 0) {
            return Ok(false);
        }
        let lane_ty = if float { types::F64 } else { types::I64 };
        let lp = self.b.create_block();
        self.b.append_block_param(lp, types::I64);
        self.b.append_block_param(lp, lane_ty);
        let body = self.b.create_block();
        let slow = self.b.create_block();
        let exit = self.b.create_block();
        self.b.append_block_param(exit, lane_ty);
        let lo = self.get(f, *from);
        let lo = self.coerce(lo, types::I64);
        let hi = self.get(f, *to);
        let hi = self.coerce(hi, types::I64);
        let acc0 = self.get(f, *init);
        let acc0 = self.coerce(acc0, lane_ty);
        self.b.ins().jump(lp, &[lo.into(), acc0.into()]);
        self.b.switch_to_block(lp);
        let p = self.b.block_params(lp)[0];
        let acc = self.b.block_params(lp)[1];
        let done = self.b.ins().icmp(IntCC::SignedGreaterThanOrEqual, p, hi);
        self.b.ins().brif(done, exit, &[acc.into()], body, &[]);
        self.b.switch_to_block(body);
        let x = self.lane(f, term, p, slow);
        let x = self.coerce(x, lane_ty);
        let next = match *fold {
            simd::REDUCE_ADD if float => self.b.ins().fadd(acc, x),
            simd::REDUCE_MUL if float => self.b.ins().fmul(acc, x),
            simd::REDUCE_ADD => {
                let (r, of) = self.b.ins().sadd_overflow(acc, x);
                self.leave_if(of, slow);
                r
            }
            simd::REDUCE_MUL => {
                let (r, of) = self.b.ins().smul_overflow(acc, x);
                self.leave_if(of, slow);
                r
            }
            simd::REDUCE_AND => self.b.ins().band(acc, x),
            simd::REDUCE_OR => self.b.ins().bor(acc, x),
            simd::REDUCE_XOR => self.b.ins().bxor(acc, x),
            k => {
                let take = self.lane_cmp(k - simd::REDUCE_TAKE, float, x, acc);
                self.b.ins().select(take, x, acc)
            }
        };
        let p1 = self.b.ins().iadd_imm_s(p, 1);
        self.b.ins().jump(lp, &[p1.into(), next.into()]);
        self.b.switch_to_block(slow);
        // The loop's result is its `dest` slot, as `VLoop` leaves it.
        let (_, frame) = self.vector_lanes(f, inst);
        let w = self.b.ins().stack_load(types::I64, lane_ty, frame, 0);
        self.b.ins().jump(exit, &[w.into()]);
        self.b.switch_to_block(exit);
        let r = self.b.block_params(exit)[0];
        self.set(f, *dest, r);
        if let Some(p) = pos {
            let zero = self.b.ins().iconst(types::I64, 0);
            self.set(f, *p, zero);
        }
        Ok(true)
    }

    /// Whether every node of `e` lowers to native lane code: word loads of
    /// arrays with a view, and the kinds [`Self::lane`] knows.
    fn lanes_native(&self, e: &compiler::native_mir::VecExpr, depth: u32) -> bool {
        use compiler::native_mir::VecExpr;
        use common::{dense, simd};
        if depth > 32 {
            return false;
        }
        match e {
            VecExpr::Splat { ty, .. } => matches!(*ty, dense::TY_I64 | dense::TY_F64),
            VecExpr::Index => true,
            VecExpr::Load { array, ty, .. } => matches!(*ty, dense::TY_I64 | dense::TY_F64) && self.views.contains_key(array),
            VecExpr::Bin { kind, lhs, rhs } => {
                let known = matches!(
                    *kind,
                    simd::IADD64 | simd::ISUB64 | simd::IMUL64 | simd::IDIV64 | simd::FADD64 | simd::FSUB64 | simd::FMUL64 | simd::FDIV64 | simd::IAND64 | simd::IOR64 | simd::IXOR64
                ) || (simd::ICMP..simd::ICMP + 6).contains(kind)
                    || (simd::FCMP..simd::FCMP + 6).contains(kind);
                known && self.lanes_native(lhs, depth + 1) && self.lanes_native(rhs, depth + 1)
            }
            VecExpr::Unary { kind, src } => matches!(*kind, simd::INEG | simd::FNEG | simd::I2F | simd::F2I) && self.lanes_native(src, depth + 1),
            VecExpr::Select { mask, then, els } => {
                self.lanes_native(mask, depth + 1) && self.lanes_native(then, depth + 1) && self.lanes_native(els, depth + 1)
            }
            VecExpr::Fma { ty, a, b, c } => {
                matches!(*ty, dense::TY_I64 | dense::TY_F64) && self.lanes_native(a, depth + 1) && self.lanes_native(b, depth + 1) && self.lanes_native(c, depth + 1)
            }
        }
    }

    /// Lane `p` of `e` (an `i64` or `f64` value); a trap or a load outside
    /// its view jumps to `slow`.
    fn lane(&mut self, f: &MirFunc, e: &compiler::native_mir::VecExpr, p: cranelift_codegen::ir::Value, slow: Block) -> cranelift_codegen::ir::Value {
        use compiler::native_mir::VecExpr;
        use common::{dense, simd};
        let int = |me: &mut Self, v| me.coerce(v, types::I64);
        let flt = |me: &mut Self, v| me.coerce(v, types::F64);
        match e {
            VecExpr::Splat { v, ty } => {
                let x = self.get(f, *v);
                self.coerce(x, if *ty == dense::TY_F64 { types::F64 } else { types::I64 })
            }
            VecExpr::Index => p,
            VecExpr::Load { array, off, ty } => {
                let o = self.get(f, *off);
                let o = int(self, o);
                let (i, of) = self.b.ins().sadd_overflow(p, o);
                self.leave_if(of, slow);
                let (fast, out, _join) = self.in_view(*array, i, false);
                self.b.switch_to_block(out);
                self.b.ins().jump(slow, &[]);
                self.b.switch_to_block(fast);
                let at = self.element(*array, i);
                let w = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
                if *ty == dense::TY_F64 { flt(self, w) } else { w }
            }
            VecExpr::Bin { kind, lhs, rhs } => {
                let a = self.lane(f, lhs, p, slow);
                let c = self.lane(f, rhs, p, slow);
                match *kind {
                    simd::FADD64 | simd::FSUB64 | simd::FMUL64 | simd::FDIV64 => {
                        let (a, c) = (flt(self, a), flt(self, c));
                        match *kind {
                            simd::FADD64 => self.b.ins().fadd(a, c),
                            simd::FSUB64 => self.b.ins().fsub(a, c),
                            simd::FMUL64 => self.b.ins().fmul(a, c),
                            _ => self.b.ins().fdiv(a, c),
                        }
                    }
                    k if (simd::FCMP..simd::FCMP + 6).contains(&k) => {
                        let (a, c) = (flt(self, a), flt(self, c));
                        let t = self.lane_cmp(k - simd::FCMP, true, a, c);
                        self.b.ins().uextend(types::I64, t)
                    }
                    k => {
                        let (a, c) = (int(self, a), int(self, c));
                        match k {
                            simd::IADD64 => {
                                let (r, of) = self.b.ins().sadd_overflow(a, c);
                                self.leave_if(of, slow);
                                r
                            }
                            simd::ISUB64 => {
                                let (r, of) = self.b.ins().ssub_overflow(a, c);
                                self.leave_if(of, slow);
                                r
                            }
                            simd::IMUL64 => {
                                let (r, of) = self.b.ins().smul_overflow(a, c);
                                self.leave_if(of, slow);
                                r
                            }
                            simd::IDIV64 => {
                                let z = self.b.ins().icmp_imm_s(IntCC::Equal, c, 0);
                                self.leave_if(z, slow);
                                let m = self.b.ins().icmp_imm_s(IntCC::Equal, a, i64::MIN);
                                let n = self.b.ins().icmp_imm_s(IntCC::Equal, c, -1);
                                let both = self.b.ins().band(m, n);
                                self.leave_if(both, slow);
                                self.b.ins().sdiv(a, c)
                            }
                            simd::IAND64 => self.b.ins().band(a, c),
                            simd::IOR64 => self.b.ins().bor(a, c),
                            simd::IXOR64 => self.b.ins().bxor(a, c),
                            k => {
                                let t = self.lane_cmp(k - simd::ICMP, false, a, c);
                                self.b.ins().uextend(types::I64, t)
                            }
                        }
                    }
                }
            }
            VecExpr::Unary { kind, src } => {
                let a = self.lane(f, src, p, slow);
                match *kind {
                    simd::INEG => {
                        let a = int(self, a);
                        let m = self.b.ins().icmp_imm_s(IntCC::Equal, a, i64::MIN);
                        self.leave_if(m, slow);
                        self.b.ins().ineg(a)
                    }
                    simd::FNEG => {
                        let a = flt(self, a);
                        self.b.ins().fneg(a)
                    }
                    simd::I2F => {
                        let a = int(self, a);
                        self.b.ins().fcvt_from_sint(types::F64, a)
                    }
                    _ => {
                        let a = flt(self, a);
                        self.b.ins().fcvt_to_sint_sat(types::I64, a)
                    }
                }
            }
            VecExpr::Select { mask, then, els } => {
                let m = self.lane(f, mask, p, slow);
                let m = int(self, m);
                let t = self.lane(f, then, p, slow);
                let e = self.lane(f, els, p, slow);
                let e = self.coerce(e, self.b.func.dfg.value_type(t));
                let c = self.b.ins().icmp_imm_s(IntCC::NotEqual, m, 0);
                self.b.ins().select(c, t, e)
            }
            VecExpr::Fma { ty, a, b, c } => {
                let x = self.lane(f, a, p, slow);
                let y = self.lane(f, b, p, slow);
                let z = self.lane(f, c, p, slow);
                if *ty == dense::TY_F64 {
                    let (x, y, z) = (flt(self, x), flt(self, y), flt(self, z));
                    let m = self.b.ins().fmul(x, y);
                    self.b.ins().fadd(m, z)
                } else {
                    let (x, y, z) = (int(self, x), int(self, y), int(self, z));
                    let (m, of) = self.b.ins().smul_overflow(x, y);
                    self.leave_if(of, slow);
                    let (r, of) = self.b.ins().sadd_overflow(m, z);
                    self.leave_if(of, slow);
                    r
                }
            }
        }
    }

    /// `a pred b` on lanes (`dense::CMP_*`), signed for ints and IEEE for
    /// floats (a NaN compares unequal and unordered), as the runtime's lanes.
    fn lane_cmp(&mut self, pred: u8, float: bool, a: cranelift_codegen::ir::Value, b: cranelift_codegen::ir::Value) -> cranelift_codegen::ir::Value {
        use common::dense;
        if float {
            let cc = match pred {
                dense::CMP_LT => FloatCC::LessThan,
                dense::CMP_LE => FloatCC::LessThanOrEqual,
                dense::CMP_GT => FloatCC::GreaterThan,
                dense::CMP_GE => FloatCC::GreaterThanOrEqual,
                dense::CMP_EQ => FloatCC::Equal,
                _ => FloatCC::NotEqual,
            };
            self.b.ins().fcmp(cc, a, b)
        } else {
            let cc = match pred {
                dense::CMP_LT => IntCC::SignedLessThan,
                dense::CMP_LE => IntCC::SignedLessThanOrEqual,
                dense::CMP_GT => IntCC::SignedGreaterThan,
                dense::CMP_GE => IntCC::SignedGreaterThanOrEqual,
                dense::CMP_EQ => IntCC::Equal,
                _ => IntCC::NotEqual,
            };
            self.b.ins().icmp(cc, a, b)
        }
    }

    /// Go to `slow` when `cond`, else on.
    fn leave_if(&mut self, cond: cranelift_codegen::ir::Value, slow: Block) {
        let next = self.b.create_block();
        self.b.ins().brif(cond, slow, &[], next, &[]);
        self.b.switch_to_block(next);
    }

    /// A `ByteScan` / `VecLoop` through the runtime's lane code (the
    /// interpreter's `VScan` / `VLoop` kernels, which need no machine): the
    /// helper's result word and the frame the slots came back in.
    fn vector_lanes(&mut self, f: &MirFunc, inst: &MirInst) -> (cranelift_codegen::ir::Value, cranelift_codegen::ir::StackSlot) {
        let op = compiler::native_mir::vector_op(inst).expect("a vector op checked at planning");
        let t = MemFlagsData::trusted();
        let n = op.inputs.len() + 2;
        let frame = self.b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
            (n * 8) as u32,
            3,
        ));
        let zero = self.b.ins().iconst(types::I64, 0);
        let start = match op.start {
            Some(v) => {
                let x = self.get(f, v);
                self.coerce(x, types::I64)
            }
            None => zero,
        };
        self.b.ins().stack_store(types::I64, start, frame, 0);
        self.b.ins().stack_store(types::I64, zero, frame, 8);
        for (i, v) in op.inputs.iter().enumerate() {
            let x = self.get(f, *v);
            let x = self.coerce(x, types::I64);
            self.b.ins().stack_store(types::I64, x, frame, ((i + 2) * 8) as i32);
        }
        let code = self.b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
            (op.body.len().max(1) * 8) as u32,
            3,
        ));
        for (i, w) in op.body.iter().enumerate() {
            let x = self.b.ins().iconst(types::I64, *w as i64);
            self.b.ins().stack_store(types::I64, x, code, (i * 8) as i32);
        }
        let (kind, lo, hi) = match inst {
            MirInst::ByteScan { from, bound, .. } => (0, *from, *bound),
            MirInst::VecLoop { from, to, .. } => (1, *from, *to),
            _ => unreachable!(),
        };
        let lo = self.get(f, lo);
        let lo = self.coerce(lo, types::I64);
        let hi = self.get(f, hi);
        let hi = self.coerce(hi, types::I64);
        let kind = self.b.ins().iconst(types::I64, kind);
        let flags = self.b.ins().iconst(types::I64, i64::from(op.flags));
        let body = self.b.ins().stack_addr(types::I64, code, 0);
        let nbody = self.b.ins().iconst(types::I64, op.body.len() as i64);
        let slots = self.b.ins().stack_addr(types::I64, frame, 0);
        let nslots = self.b.ins().iconst(types::I64, n as i64);
        let call = self.b.ins().call(self.vector, &[self.ctx, kind, flags, body, nbody, slots, nslots, lo, hi]);
        let w = self.b.inst_results(call)[0];
        let flag = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_BAIL);
        self.bail_if(flag, TRAP_HELPER);
        (w, frame)
    }

    fn bail_if(&mut self, cond: cranelift_codegen::ir::Value, code: u64) {
        let next = self.b.create_block();
        let args = self.bail_args(code);
        self.b.ins().brif(cond, self.bail, &args, next, &[]);
        self.b.switch_to_block(next);
    }

    fn bail_args(&mut self, code: u64) -> [cranelift_codegen::ir::BlockArg; 2] {
        let site = self.site();
        let code = self.b.ins().iconst(types::I64, code as i64);
        let site = self.b.ins().iconst(types::I64, site);
        [code.into(), site.into()]
    }

    /// The current op's site: its index into `sites`, or [`NO_SITE`] when
    /// its location is unknown.
    fn site(&mut self) -> i64 {
        if let Some(s) = self.cur_site {
            return s;
        }
        let s = match (self.entry_pc, self.cur_loc) {
            // Without a location the site still names its function, as a
            // runtime frame without one does.
            (Some(entry_pc), loc) => {
                let loc = loc.unwrap_or(common::DebugLoc::unknown());
                self.sites.push(NativeSite { entry_pc, loc });
                (self.sites.len() - 1) as i64
            }
            _ => NO_SITE as i64,
        };
        self.cur_site = Some(s);
        s
    }

    /// Lower the next op at `loc` (`None`: no site).
    fn at(&mut self, loc: Option<common::DebugLoc>) {
        self.cur_loc = loc;
        self.cur_site = None;
    }

    fn body(&mut self, f: &MirFunc) -> Result<(), String> {
        self.barriers = barrier_pairs(f);
        self.opaque = opaque_words(f);
        self.ptr_words = pointer_words(f);
        self.root_handles = root_handles(f);
        self.pair_words = pair_words(f, &self.ptr_words);
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
        if self.is_pad {
            self.homes_ptr = params.first().copied();
        } else {
            for (&p, &x) in f.params.iter().zip(&params) {
                self.set(f, p, x);
                self.view_of(f, p);
            }
            if !f.homes.is_empty() {
                // Homes start zeroed (every `defer` disarmed), as frame
                // words past the registers do.
                let slot = self.b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    (f.homes.len() * 8) as u32,
                    3,
                ));
                let zero = self.b.ins().iconst(types::I64, 0);
                for i in 0..f.homes.len() {
                    self.b.ins().stack_store(types::I64, zero, slot, (i * 8) as i32);
                }
                self.homes_slot = Some(slot);
                self.heap_homes = (0..f.homes.len() as u16).filter(|&h| heap_word(f.homes[h as usize])).collect();
                if f.opaque_words {
                    self.amb_homes = (0..f.homes.len() as u16).filter(|&h| f.homes[h as usize] == MirTy::I64).collect();
                }
            }
            self.pad_fn = self.body_index.and_then(|i| self.pad_refs.get(&i).copied());
        }
        for blk in &f.blocks {
            let cl = self.b.create_block();
            self.blocks.insert(blk.id, cl);
        }
        // Trap before the stack runs out. No site of its own: the caller's
        // call names the frame.
        let sp = self.b.ins().get_stack_pointer(types::I64);
        let limit = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_STACK_LIMIT);
        let low = self.b.ins().icmp(IntCC::UnsignedLessThan, sp, limit);
        self.at(None);
        self.bail_if(low, TRAP_STACK);
        let gc = self.body_index.is_some_and(|i| self.plan.may_gc.contains(&i));
        // Live sets by block position: into the block past its phis, and
        // at each op (what lives after it and what it reads, which the
        // call it makes may move).
        let (live_in, live_after) = compiler::native_mir::liveness(f);
        let held = held_words(f);
        let past_phis = |bi: usize| -> HashSet<ValueId> {
            let phis = f.blocks[bi].insts.iter().take_while(|i| i.is_phi()).count();
            let mut live = if phis == 0 { live_in[bi].clone() } else { live_after[bi][phis - 1].clone() };
            live.extend(held[bi][phis].iter().copied());
            live
        };
        let entry_at = f.blocks.iter().position(|b| b.id == f.entry);
        if gc {
            self.live = entry_at.map(past_phis);
            self.safepoint(f);
        }
        // Loop headers (the targets of edges back in block order) are
        // safepoints too.
        let order: HashMap<BlockId, usize> = f.blocks.iter().enumerate().map(|(i, b)| (b.id, i)).collect();
        let mut headers = HashSet::new();
        if gc {
            for (i, blk) in f.blocks.iter().enumerate() {
                let targets: Vec<BlockId> = match &blk.term {
                    Some(Terminator::Jump { dest }) => vec![*dest],
                    Some(Terminator::Br { taken, not_taken, .. }) => vec![*taken, *not_taken],
                    Some(Terminator::JumpIfMatch { taken, not_taken, .. }) => vec![*taken, *not_taken],
                    _ => Vec::new(),
                };
                headers.extend(targets.into_iter().filter(|t| order.get(t).is_some_and(|&j| j <= i)));
            }
        }
        let first = self.blocks[&f.entry];
        self.b.ins().jump(first, &[]);

        for (bi, blk) in f.blocks.iter().enumerate() {
            let cl = self.blocks[&blk.id];
            self.b.switch_to_block(cl);
            if headers.contains(&blk.id) {
                self.at(None);
                self.live = Some(past_phis(bi));
                self.safepoint(f);
            }
            // A `MatchPayload` of a payload the block's `JumpIfMatch` binds
            // belongs to the taken edge, where the terminator binds it: run here, it would read the payload
            // of whatever variant the word holds.
            let edge_bound: &[ValueId] = match &blk.term {
                Some(Terminator::JumpIfMatch { payloads, .. }) => payloads,
                _ => &[],
            };
            let tail = self.body_index.is_some_and(|i| self.plan.tail_sites.get(&i).is_some_and(|b| b.contains(&blk.id)));
            for (ii, inst) in blk.insts.iter().enumerate() {
                if let MirInst::MatchPayload { dest, .. } = inst
                    && edge_bound.contains(dest)
                {
                    continue;
                }
                if tail && ii + 1 == blk.insts.len() {
                    self.at(Some(f.op_loc(inst.dest())));
                    self.tail_call(f, inst)?;
                    continue;
                }
                let mut live = live_after[bi][ii].clone();
                live.extend(inst.operands());
                live.extend(held[bi][ii].iter().copied());
                self.live = Some(live);
                self.at(Some(f.op_loc(inst.dest())));
                self.op_words.clear();
                if !matches!(inst, MirInst::Call { .. }) {
                    let held = inst.operands().into_iter().filter(|v| heap_word(ty_of(f, *v)) || self.ptr_words.contains(v));
                    self.op_words.extend(held);
                    self.op_words.sort_unstable_by_key(|v| v.0);
                    self.op_words.dedup();
                }
                self.inst(f, inst)?;
                self.op_words.clear();
            }
            self.live = None;
            self.at(None);
            if !tail {
                self.term(f, blk.id, blk.term.as_ref().ok_or("no terminator")?)?;
            }
        }

        // Bail or panic (`coil_native_unwind` decides), return a dummy word.
        self.b.switch_to_block(self.bail);
        let p = self.b.block_params(self.bail).to_vec();
        self.b.ins().call(self.unwind, &[self.ctx, p[0], p[1]]);
        // After a panic, this frame's armed `defer`s run once the
        // runtime reported it: hand it the pad and the homes.
        if let (Some(pad), Some(slot)) = (self.pad_fn, self.homes_slot) {
            let fp = self.b.ins().func_addr(types::I64, pad);
            let at = self.b.ins().stack_addr(types::I64, slot, 0);
            // An opaque body's `int` homes go in the record too, which then
            // roots every home ambiguously.
            let amb = if self.amb_homes.is_empty() { 0 } else { ROOTS_AMBIGUOUS };
            let n = self.b.ins().iconst(types::I64, (f.homes.len() as u64 | amb) as i64);
            let mask = self.heap_homes.iter().chain(&self.amb_homes).fold(0u64, |m, &h| m | (1 << h));
            let mask = self.b.ins().iconst(types::I64, mask as i64);
            self.b.ins().call(self.defer, &[self.ctx, fp, at, n, mask]);
        }
        let mut zeros = Vec::new();
        for t in std::iter::once(self.ret).chain(self.ret_hi) {
            zeros.push(if t == types::F64 { self.b.ins().f64const(0.0) } else { self.b.ins().iconst(types::I64, 0) });
        }
        self.b.ins().return_(&zeros);
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
                        // `!=` is true when unordered.
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
                        self.bail_if(min, TRAP_OVERFLOW);
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
                    // Rust `as`: saturating, NaN is 0, as the runtime.
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
            MirInst::CodePtr { dest, target } => {
                let pc = *self.plan.pcs.get(target).ok_or("code pointer of a body with no entry PC")?;
                let x = self.b.ins().iconst(types::I64, i64::from(pc));
                self.set(f, *dest, x);
            }
            MirInst::HomeLoad { dest, home } => {
                let (base, off) = self.home_at(*home)?;
                let x = self.b.ins().load(types::I64, MemFlagsData::trusted(), base, off);
                self.set_word(f, *dest, x);
                self.view_of(f, *dest);
            }
            MirInst::HomeStore { dest, home, value } => {
                let v = self.get(f, *value);
                let v = self.coerce(v, types::I64);
                let (base, off) = self.home_at(*home)?;
                self.b.ins().store(MemFlagsData::trusted(), v, base, off);
                if numeric(ty_of(f, *dest)) {
                    let zero = self.b.ins().iconst(types::I64, 0);
                    self.set(f, *dest, zero);
                }
            }
            MirInst::KeepAlive { dest, value } => {
                self.keep_word(f, *value);
                if numeric(ty_of(f, *dest)) {
                    let zero = self.b.ins().iconst(types::I64, 0);
                    self.set(f, *dest, zero);
                }
            }
            MirInst::Probe { dest, id } => {
                let id = self.b.ins().iconst(types::I64, i64::from(*id));
                self.b.ins().call(self.probe, &[self.ctx, id]);
                let zero = self.b.ins().iconst(types::I64, 0);
                self.set(f, *dest, zero);
            }
            MirInst::Call { dest, dest_hi, target, args } => {
                // A `drop` called as a method: claim its finalizer first, and skip the call when it ran.
                let drop_pc = self
                    .by_label
                    .get(target)
                    .filter(|c| args.len() == 1 && self.plan.drops.contains(c))
                    .and_then(|_| self.plan.pcs.get(target).copied());
                let Some(pc) = drop_pc else {
                    return self.call(f, *dest, *dest_hi, *target, args);
                };
                let pcv = self.b.ins().iconst(types::I64, i64::from(pc));
                let w = self.get(f, args[0]);
                let w = self.coerce(w, types::I64);
                let c = self.b.ins().call(self.claim, &[self.ctx, pcv, w]);
                let run = self.b.inst_results(c)[0];
                let (go, skip, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().brif(run, go, &[], skip, &[]);
                self.b.switch_to_block(skip);
                let zero = self.b.ins().iconst(types::I64, 0);
                self.set(f, *dest, zero);
                if let Some(h) = dest_hi {
                    self.set(f, *h, zero);
                }
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(go);
                self.call(f, *dest, *dest_hi, *target, args)?;
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(join);
                self.view_of(f, *dest);
            }
            MirInst::CallIndirect { dest, callee, args, dicts } => {
                let c = self.get(f, *callee);
                let c = self.coerce(c, types::I64);
                let p = self.words_slot(f, args);
                let n = self.b.ins().iconst(types::I64, args.len() as i64);
                let d = self.b.ins().iconst(types::I64, i64::from(*dicts));
                let roots = self.push_roots(f);
                let prev = self.push_call();
                let call = self.b.ins().call(self.call_value, &[self.ctx, c, p, n, d]);
                self.pop_call(prev);
                let w = self.b.inst_results(call)[0];
                self.pop_roots(roots);
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                self.drop_views();
                self.set_word(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::ResumeCoro { dest, handle, send } => {
                // A generator's body runs on the runtime, in a nested execution
                // that ends at its next yield or return.
                let h = self.get(f, *handle);
                let h = self.coerce(h, types::I64);
                let (sv, has) = match send {
                    Some(s) => {
                        let v = self.get(f, *s);
                        (self.coerce(v, types::I64), 1)
                    }
                    None => (self.b.ins().iconst(types::I64, 0), 0),
                };
                let has = self.b.ins().iconst(types::I64, has);
                let roots = self.push_roots(f);
                let prev = self.push_call();
                let call = self.b.ins().call(self.resume, &[self.ctx, h, sv, has]);
                self.pop_call(prev);
                let w = self.b.inst_results(call)[0];
                self.pop_roots(roots);
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                self.drop_views();
                self.set_word(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::ByteScan { .. } | MirInst::VecLoop { .. } => {
                // A pure reduce compiles to a native loop; the rest runs the
                // runtime's `VScan` / `VLoop` kernels (`coil_native_vector`).
                if self.native_reduce(f, inst)? {
                    return Ok(());
                }
                let (w, frame) = self.vector_lanes(f, inst);
                match inst {
                    MirInst::ByteScan { dest, .. } => self.set(f, *dest, w),
                    MirInst::VecLoop { dest, pos, .. } => {
                        // A push may have moved the array's storage.
                        self.drop_views();
                        let d = self.b.ins().stack_load(types::I64, types::I64, frame, 0);
                        self.set(f, *dest, d);
                        if let Some(p) = pos {
                            let x = self.b.ins().stack_load(types::I64, types::I64, frame, 8);
                            self.set(f, *p, x);
                        }
                    }
                    _ => unreachable!(),
                }
            }
            MirInst::Ffi { dest, op, operand, args } => {
                let o = self.b.ins().iconst(types::I64, i64::from(op.code()));
                let x = self.b.ins().iconst(types::I64, i64::from(*operand));
                let p = self.words_slot(f, args);
                let n = self.b.ins().iconst(types::I64, args.len() as i64);
                let roots = self.push_roots(f);
                let prev = self.push_call();
                let call = self.b.ins().call(self.ffi, &[self.ctx, o, x, p, n]);
                let w = self.b.inst_results(call)[0];
                self.pop_call(prev);
                self.pop_roots(roots);
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                self.drop_views();
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::Yield { dest, value, from: true, .. } => {
                // `yield from h`: resume `h` (sending what this generator's
                // resumer sent) and yield what it yields until it is done.
                let t = MemFlagsData::trusted();
                let (lp, more, exit) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.append_block_param(lp, types::I64);
                let zero = self.b.ins().iconst(types::I64, 0);
                self.b.ins().jump(lp, &[zero.into()]);
                self.b.switch_to_block(lp);
                let send = self.b.block_params(lp)[0];
                let h = self.get(f, *value);
                let h = self.coerce(h, types::I64);
                let has = self.b.ins().iconst(types::I64, 1);
                let roots = self.push_roots(f);
                let prev = self.push_call();
                let call = self.b.ins().call(self.resume, &[self.ctx, h, send, has]);
                self.pop_call(prev);
                let w = self.b.inst_results(call)[0];
                self.pop_roots(roots);
                let flag = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                self.drop_views();
                let h = self.get(f, *value);
                let h = self.coerce(h, types::I64);
                let call = self.b.ins().call(self.coro_done, &[self.ctx, h]);
                let done = self.b.inst_results(call)[0];
                self.b.ins().brif(done, exit, &[], more, &[]);
                self.b.switch_to_block(more);
                let roots = self.push_roots(f);
                let prev = self.push_call();
                let call = self.b.ins().call(self.gen_yield, &[self.ctx, w]);
                self.pop_call(prev);
                let sent = self.b.inst_results(call)[0];
                self.pop_roots(roots);
                // A close unwinds from the yield (`coil_native_yield`).
                let flag = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                self.drop_views();
                self.b.ins().jump(lp, &[sent.into()]);
                self.b.switch_to_block(exit);
                let zero = self.b.ins().iconst(types::I64, 0);
                self.set_word(f, *dest, zero);
            }
            MirInst::Yield { dest, value, .. } => {
                // The fiber suspends here; the resumer forwards the frame's
                // words while it is away (they are this call's roots).
                let v = self.get(f, *value);
                let v = self.coerce(v, types::I64);
                let roots = self.push_roots(f);
                let prev = self.push_call();
                let call = self.b.ins().call(self.gen_yield, &[self.ctx, v]);
                self.pop_call(prev);
                let w = self.b.inst_results(call)[0];
                self.pop_roots(roots);
                // A close unwinds from the yield (`coil_native_yield`).
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                self.drop_views();
                self.set_word(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::DoneCoro { dest, handle } => {
                let h = self.get(f, *handle);
                let h = self.coerce(h, types::I64);
                let call = self.b.ins().call(self.coro_done, &[self.ctx, h]);
                let w = self.b.inst_results(call)[0];
                self.set_word(f, *dest, w);
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
                self.bail_if(flag, TRAP_HELPER);
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
                // In place.
                self.b.switch_to_block(fast);
                let at = self.element(*array, i);
                self.b.ins().store(MemFlagsData::trusted(), v, at, 0);
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(slow);
                self.b.ins().call(self.store_index, &[self.ctx, a, i, v]);
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
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
                self.bail_if(flag, TRAP_HELPER);
                self.set(f, *dest, w);
            }
            MirInst::Alloc { dest, kind, elems, kinds } => {
                use machine::native::{ALLOC_ARRAY, ALLOC_ENUM, ALLOC_OBJECT, ALLOC_TUPLE};
                // Declaration order: an enum's elems are in push order and
                // its payload starts with the last push, as `DenseMake`.
                let mut words = elems.clone();
                if matches!(kind, MirAllocKind::Enum { .. } | MirAllocKind::DropEnum { .. }) {
                    words.reverse();
                }
                // An opaque body's `int` words may be objects: their kinds
                // stay unknown.
                let packed = || if f.opaque_words { 0 } else { common::pack_word_kinds(words.iter().map(|w| word_kind(ty_of(f, *w)))) };
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
                    MirAllocKind::DropEnum { tag, .. } => {
                        let k = if *kinds != 0 { *kinds } else { packed() };
                        (machine::native::ALLOC_DROP_ENUM, words.len() as u64 + 1, u64::from(tag) | (u64::from(k) << 32))
                    }
                    MirAllocKind::Fn { target, operand, .. } => {
                        let pc = *self.plan.pcs.get(&target).ok_or("function value of a body with no entry PC")?;
                        (machine::native::ALLOC_FN, words.len() as u64 + 1, u64::from(pc) | (u64::from(operand) << 32))
                    }
                    MirAllocKind::Coro { target } => {
                        let pc = *self.plan.pcs.get(&target).ok_or("generator of a body with no entry PC")?;
                        (machine::native::ALLOC_CORO, words.len() as u64, u64::from(pc))
                    }
                    MirAllocKind::PolyFn { target } | MirAllocKind::PolyFnCapture { target, .. } => {
                        let pc = *self.plan.pcs.get(&target).ok_or("function value of a body with no entry PC")?;
                        let params = if let MirAllocKind::PolyFnCapture { params, .. } = *kind { params } else { 0 };
                        (machine::native::ALLOC_POLY_FN, words.len() as u64, u64::from(pc) | (u64::from(params) << 32))
                    }
                };
                let inline = inline_shape(*kind, words.len(), extra).and_then(machine::native::inline_object);
                let join = inline.map(|obj| {
                    let join = self.b.create_block();
                    self.b.append_block_param(join, types::I64);
                    self.inline_alloc(f, &obj, *kind, &words, join);
                    join
                });
                let p = match *kind {
                    MirAllocKind::Fn { mask, .. } => self.words_slot_with(f, &words, Some(i64::from(mask))),
                    MirAllocKind::DropEnum { type_id, .. } => self.words_slot_with(f, &words, Some(i64::from(type_id))),
                    _ => self.words_slot(f, &words),
                };
                let code = self.b.ins().iconst(types::I64, code as i64);
                let n = self.b.ins().iconst(types::I64, n as i64);
                let extra = self.b.ins().iconst(types::I64, extra as i64);
                let call = self.b.ins().call(self.alloc, &[self.ctx, code, p, n, extra]);
                let mut w = self.b.inst_results(call)[0];
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                if let Some(join) = join {
                    self.b.ins().jump(join, &[w.into()]);
                    self.b.switch_to_block(join);
                    w = self.b.block_params(join)[0];
                }
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
                self.bail_if(flag, TRAP_HELPER);
                self.drop_views();
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::String { dest, idx } => {
                let i = self.b.ins().iconst(types::I64, i64::from(*idx));
                let call = self.b.ins().call(self.string, &[self.ctx, i]);
                let w = self.b.inst_results(call)[0];
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                self.set(f, *dest, w);
            }
            // A pad's end: the runtime goes on unwinding once the pad returns.
            MirInst::HostInvoke { dest, native_id, .. } if *native_id == common::UNWIND_RESUME_ID => {
                if numeric(ty_of(f, *dest)) {
                    let zero = self.b.ins().iconst(types::I64, 0);
                    self.set(f, *dest, zero);
                }
            }
            MirInst::HostInvoke { dest, native_id, layout, args } => {
                let fp = self.b.ins().iconst(types::I64, i64::from(*native_id));
                let lay = self.b.ins().iconst(types::I64, i64::from(*layout));
                let p = self.words_slot(f, args);
                let n = self.b.ins().iconst(types::I64, args.len() as i64);
                // `gc_collect` collects, and a host call through the runtime may:
                // this frame's words are roots.
                let collect = machine::native::direct_host_natives().get(usize::from(*native_id)).is_some_and(|h| h.collect || h.on_machine);
                let roots = if collect { self.push_roots(f) } else { None };
                let prev = if collect { Some(self.push_call()) } else { None };
                let call = self.b.ins().call(self.host, &[self.ctx, fp, lay, p, n]);
                let w = self.b.inst_results(call)[0];
                if let Some(prev) = prev {
                    self.pop_call(prev);
                }
                self.pop_roots(roots);
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                self.drop_views();
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::Panic { dest, msg } => {
                let v = self.get(f, *msg);
                let v = self.coerce(v, types::I64);
                self.b.ins().call(self.panic, &[self.ctx, v]);
                let one = self.b.ins().iconst(types::I64, 1);
                self.bail_if(one, TRAP_HELPER);
                if numeric(ty_of(f, *dest)) {
                    let zero = self.b.ins().iconst(types::I64, 0);
                    self.set(f, *dest, zero);
                }
            }
            MirInst::Print { dest, src } => {
                let v = self.get(f, *src);
                let v = self.coerce(v, types::I64);
                let call = self.b.ins().call(self.print, &[self.ctx, v]);
                let w = self.b.inst_results(call)[0];
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                self.set(f, *dest, w);
            }
            MirInst::FreeEarly { dest, value, deep } => {
                // With finalizers in the program the free may run `drop`s
                // on the runtime (and collect): root this frame's words then.
                let t = MemFlagsData::trusted();
                let fin = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_HAS_FINALIZERS);
                let (slow, fast, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().brif(fin, slow, &[], fast, &[]);
                self.b.set_cold_block(slow);
                self.b.switch_to_block(slow);
                let v = self.get(f, *value);
                let v = self.coerce(v, types::I64);
                let d = self.b.ins().iconst(types::I64, i64::from(*deep));
                let roots = self.push_roots(f);
                let prev = self.push_call();
                self.b.ins().call(self.free_early, &[self.ctx, v, d]);
                self.pop_call(prev);
                self.pop_roots(roots);
                self.drop_views();
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(fast);
                let v = self.get(f, *value);
                let v = self.coerce(v, types::I64);
                let d = self.b.ins().iconst(types::I64, i64::from(*deep));
                self.b.ins().call(self.free_early, &[self.ctx, v, d]);
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(join);
                let flag = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                if numeric(ty_of(f, *dest)) {
                    let zero = self.b.ins().iconst(types::I64, 0);
                    self.set(f, *dest, zero);
                }
            }
            MirInst::Format { dest, fmt, args } => {
                let p = self.words_slot(f, args);
                let fw = self.get(f, *fmt);
                let n = self.b.ins().iconst(types::I64, args.len() as i64);
                let call = self.b.ins().call(self.format, &[self.ctx, fw, p, n]);
                let w = self.b.inst_results(call)[0];
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                self.set(f, *dest, w);
            }
            MirInst::Stringify { dest, src, tag } => {
                let v = self.get(f, *src);
                let v = self.coerce(v, types::I64);
                // The instruction's packed operand.
                let op = tag
                    .and_then(|t| common::ValueTag::from_u16(u16::try_from(t).ok()?))
                    .map_or(0, common::stringify_operand);
                let op = self.b.ins().iconst(types::I64, i64::from(op));
                let call = self.b.ins().call(self.stringify, &[self.ctx, v, op]);
                let w = self.b.inst_results(call)[0];
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                self.set(f, *dest, w);
            }
            MirInst::StrEq { dest, lhs, rhs, ne } => {
                let a = self.get(f, *lhs);
                let a = self.coerce(a, types::I64);
                let c = self.get(f, *rhs);
                let c = self.coerce(c, types::I64);
                let call = self.b.ins().call(self.eq, &[self.ctx, a, c]);
                let w = self.b.inst_results(call)[0];
                let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
                self.bail_if(flag, TRAP_HELPER);
                let w = if *ne { self.b.ins().bxor_imm_u(w, 1) } else { w };
                self.set(f, *dest, w);
            }
            MirInst::LoadStatic { dest, slot } => {
                let at = self.static_at(*slot);
                let w = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
                self.set_word(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::StoreStatic { dest, slot, value } => {
                let v = self.get(f, *value);
                let v = self.coerce(v, types::I64);
                let at = self.static_at(*slot);
                self.b.ins().store(MemFlagsData::trusted(), v, at, 0);
                let z = self.b.ins().iconst(types::I64, 0);
                self.set(f, *dest, z);
            }
            MirInst::ClearWord { dest, .. } => {
                let z = self.b.ins().iconst(types::I64, 0);
                self.set(f, *dest, z);
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
                self.bail_if(flag, TRAP_HELPER);
                self.set_word(f, *dest, w);
                self.view_of(f, *dest);
            }
            other => return Err(format!("instruction {}", inst_name(other))),
        }
        Ok(())
    }

    /// The address of static `slot`, or bail when the machine has fewer
    /// statics (the runtime reads `0` there and drops the store; never seen).
    fn static_at(&mut self, slot: u32) -> cranelift_codegen::ir::Value {
        let t = MemFlagsData::trusted();
        let len = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_STATICS_LEN);
        let out = self.b.ins().icmp_imm_u(IntCC::UnsignedLessThanOrEqual, len, i64::from(slot));
        self.bail_if(out, TRAP_HELPER);
        let base = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_STATICS);
        self.b.ins().iadd_imm_u(base, i64::from(slot) * 8)
    }

    /// The callee at `target` and the words of a call of it on `args`.
    fn call_words(&mut self, f: &MirFunc, target: Label, args: &[ValueId]) -> Result<(usize, FuncRef, Vec<cranelift_codegen::ir::Value>), String> {
        let callee = *self.by_label.get(&target).ok_or("unknown callee")?;
        let fref = *self.module_refs.get(&callee).ok_or("callee not compiled")?;
        let sig = self.b.func.dfg.ext_funcs[fref].signature;
        let params: Vec<Type> = self.b.func.dfg.signatures[sig].params.iter().map(|p| p.value_type).collect();
        let used = self.plan.used_params.get(&callee).copied().unwrap_or(params.len() - 1);
        if args.len() < used {
            return Err(format!("call of {} words to a body that reads {used}", args.len()));
        }
        let mut vals = vec![self.ctx];
        for (i, &want) in params[1..].iter().enumerate() {
            let x = match args.get(i) {
                Some(a) => self.get(f, *a),
                None => self.b.ins().iconst(types::I64, 0),
            };
            vals.push(self.coerce(x, want));
        }
        Ok((callee, fref, vals))
    }

    /// A call that takes this frame ([`Plan::tail_sites`]): the callee
    /// returns to this body's caller, so nothing here runs after it.
    fn tail_call(&mut self, f: &MirFunc, inst: &MirInst) -> Result<(), String> {
        let MirInst::Call { target, args, .. } = inst else {
            return Err("tail site without a call".to_string());
        };
        let (_, fref, vals) = self.call_words(f, *target, args)?;
        self.b.ins().return_call(fref, &vals);
        Ok(())
    }

    /// A direct call of the body at `target`.
    fn call(&mut self, f: &MirFunc, dest: ValueId, dest_hi: Option<ValueId>, target: Label, args: &[ValueId]) -> Result<(), String> {
        let (dest, dest_hi) = (&dest, &dest_hi);
        let (callee, fref, vals) = self.call_words(f, target, args)?;
        let roots = if self.plan.may_gc.contains(&callee) { self.push_roots(f) } else { None };
        let prev = if self.plan.runtime_reach.contains(&callee) { Some(self.push_call()) } else { None };
        let call = self.b.ins().call(fref, &vals);
        let results = self.b.inst_results(call).to_vec();
        if let Some(prev) = prev {
            self.pop_call(prev);
        }
        self.pop_roots(roots);
        let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
        self.bail_if(flag, TRAP_HELPER);
        if self.plan.moves.contains(&callee) {
            self.drop_views();
        }
        self.set(f, *dest, results[0]);
        if let Some(h) = dest_hi {
            let x = *results.get(1).ok_or("two-word call of a one-word body")?;
            self.set(f, *h, x);
        }
        self.view_of(f, *dest);
        Ok(())
    }

    /// Before a call that may run interpreted code: link this frame's call site
    /// into `NativeCtx::calls`, so a panic the runtime reports meanwhile names
    /// this frame in its backtrace. Returns the chain's previous head.
    fn push_call(&mut self) -> cranelift_codegen::ir::Value {
        let slot = match self.calls_slot {
            Some(s) => s,
            None => {
                let s = self.b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    16,
                    3,
                ));
                self.calls_slot = Some(s);
                s
            }
        };
        let t = MemFlagsData::trusted();
        let prev = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_CALLS);
        self.b.ins().stack_store(types::I64, prev, slot, 0);
        let site = self.site();
        let site = self.b.ins().iconst(types::I64, site);
        self.b.ins().stack_store(types::I64, site, slot, 8);
        let at = self.b.ins().stack_addr(types::I64, slot, 0);
        self.b.ins().store(t, at, self.ctx, NATIVE_CTX_CALLS);
        prev
    }

    /// Unlink [`Self::push_call`]'s record.
    fn pop_call(&mut self, prev: cranelift_codegen::ir::Value) {
        self.b.ins().store(MemFlagsData::trusted(), prev, self.ctx, NATIVE_CTX_CALLS);
    }

    /// Base address and offset of home `h`.
    fn home_at(&mut self, h: u16) -> Result<(cranelift_codegen::ir::Value, i32), String> {
        let off = i32::from(h) * 8;
        if let Some(p) = self.homes_ptr {
            return Ok((p, off));
        }
        let slot = self.homes_slot.ok_or("home outside a body with homes")?;
        Ok((self.b.ins().stack_addr(types::I64, slot, 0), off))
    }

    /// The words of `vs` in a new stack slot, in order; its address.
    /// Inline allocation of `obj` (`machine::native::inline_object`): with
    /// the heap's gate open (and, for an instance, no `drop` on its type)
    /// and room in its nursery chunk, bump the class's cursor, write the
    /// slot's image with `words` in place and count the object, then jump
    /// to `join` with its address. Otherwise the code after this (the
    /// current block, on return) calls the helper, which refills.
    fn inline_alloc(&mut self, f: &MirFunc, obj: &machine::native::InlineObject, kind: MirAllocKind, words: &[ValueId], join: Block) {
        use machine::native::{
            BUMP_BYTES, BUMP_END, BUMP_NEXT, COUNTS_ALLOC_BYTES, COUNTS_FIN_LEN, COUNTS_FIN_WORDS, COUNTS_INLINE_GATE,
            COUNTS_LIVE_COUNT, COUNTS_YOUNG_BYTES, NATIVE_CTX_ALLOC_BUMPS, NATIVE_CTX_ALLOC_COUNTS,
        };
        let mem = MemFlagsData::trusted();
        let slow = self.b.create_block();
        let bump = self.b.create_block();
        let fast = self.b.create_block();
        let counts = self.b.ins().load(types::I64, mem, self.ctx, NATIVE_CTX_ALLOC_COUNTS);
        let gate = self.b.ins().load(types::I64, mem, counts, COUNTS_INLINE_GATE);
        if let MirAllocKind::Object { type_id, .. } = kind {
            // A type with a `drop` stays out of the nursery: the helper
            // allocates it old and lists it.
            let check = self.b.create_block();
            self.b.ins().brif(gate, check, &[], slow, &[]);
            self.b.switch_to_block(check);
            let word = i64::from(type_id / 64);
            let len = self.b.ins().load(types::I64, mem, counts, COUNTS_FIN_LEN);
            let past = self.b.ins().icmp_imm_s(IntCC::UnsignedLessThanOrEqual, len, word);
            let test = self.b.create_block();
            self.b.ins().brif(past, bump, &[], test, &[]);
            self.b.switch_to_block(test);
            let base = self.b.ins().load(types::I64, mem, counts, COUNTS_FIN_WORDS);
            let bits = self.b.ins().load(types::I64, mem, base, (word * 8) as i32);
            let bit = self.b.ins().band_imm_s(bits, 1_i64 << (type_id % 64));
            self.b.ins().brif(bit, slow, &[], bump, &[]);
        } else {
            self.b.ins().brif(gate, bump, &[], slow, &[]);
        }
        self.b.switch_to_block(bump);
        let bumps = self.b.ins().load(types::I64, mem, self.ctx, NATIVE_CTX_ALLOC_BUMPS);
        let cursor = self.b.ins().iadd_imm_s(bumps, i64::from(obj.class) * i64::from(BUMP_BYTES));
        let at = self.b.ins().load(types::I64, mem, cursor, BUMP_NEXT);
        let end = self.b.ins().load(types::I64, mem, cursor, BUMP_END);
        let next = self.b.ins().iadd_imm_s(at, i64::from(obj.slot));
        let fits = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, next, end);
        self.b.ins().brif(fits, fast, &[], slow, &[]);
        self.b.switch_to_block(fast);
        self.b.ins().store(mem, next, cursor, BUMP_NEXT);
        let first = obj.words_at as usize;
        for (i, &img) in obj.image.iter().enumerate() {
            let x = match i.checked_sub(first).and_then(|k| words.get(k)) {
                Some(&v) => {
                    let x = self.get(f, v);
                    self.coerce(x, types::I64)
                }
                None => self.b.ins().iconst(types::I64, img as i64),
            };
            self.b.ins().store(mem, x, at, (i * 8) as i32);
        }
        for (off, by) in [(COUNTS_ALLOC_BYTES, i64::from(obj.size)), (COUNTS_YOUNG_BYTES, i64::from(obj.size)), (COUNTS_LIVE_COUNT, 1)] {
            let c = self.b.ins().load(types::I64, mem, counts, off);
            let c = self.b.ins().iadd_imm_s(c, by);
            self.b.ins().store(mem, c, counts, off);
        }
        self.b.ins().jump(join, &[at.into()]);
        self.b.switch_to_block(slow);
    }

    /// Before a return: use every `gc::root` handle, so the stack maps keep
    /// each one (and what it roots) alive until the body returns, as an
    /// unused `let r = root(h)` asks.
    fn keep_roots(&mut self, f: &MirFunc) {
        for v in self.root_handles.clone() {
            self.keep_word(f, v);
        }
    }

    /// A use of `v` that does nothing else: its word stays live to here,
    /// so the stack maps hold it.
    fn keep_word(&mut self, f: &MirFunc, v: ValueId) {
        let slot = *self.keep_slot.get_or_insert_with(|| {
            self.b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(cranelift_codegen::ir::StackSlotKind::ExplicitSlot, 8, 3))
        });
        let x = self.get(f, v);
        let x = self.coerce(x, types::I64);
        self.b.ins().stack_store(types::I64, x, slot, 0);
    }

    fn words_slot(&mut self, f: &MirFunc, vs: &[ValueId]) -> cranelift_codegen::ir::Value {
        self.words_slot_with(f, vs, None)
    }

    /// [`Self::words_slot`] with the constant `last` after the words.
    fn words_slot_with(&mut self, f: &MirFunc, vs: &[ValueId], last: Option<i64>) -> cranelift_codegen::ir::Value {
        let n = vs.len() + usize::from(last.is_some());
        let slot = self.b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
            (n.max(1) * 8) as u32,
            3,
        ));
        for (i, v) in vs.iter().enumerate() {
            let x = self.get(f, *v);
            let x = self.coerce(x, types::I64);
            self.b.ins().stack_store(types::I64, x, slot, (i * 8) as i32);
        }
        if let Some(c) = last {
            let x = self.b.ins().iconst(types::I64, c);
            self.b.ins().stack_store(types::I64, x, slot, (vs.len() * 8) as i32);
        }
        self.b.ins().stack_addr(types::I64, slot, 0)
    }

    /// Payload word `index` of the matched `scrutinee` (word `s`): a niche
    /// word is its own payload; a boxed
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
        self.bail_if(flag, TRAP_HELPER);
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

    /// Define `v` from a word read out of the heap: a `bool` word is
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
                self.bail_if(of, TRAP_OVERFLOW);
                r
            }
            MirBinOp::Sub => {
                let (r, of) = self.b.ins().ssub_overflow(a, c);
                self.bail_if(of, TRAP_OVERFLOW);
                r
            }
            MirBinOp::Mul => {
                let (r, of) = self.b.ins().smul_overflow(a, c);
                self.bail_if(of, TRAP_OVERFLOW);
                r
            }
            MirBinOp::Div => {
                let z = self.b.ins().icmp_imm_s(IntCC::Equal, c, 0);
                self.bail_if(z, TRAP_DIV_ZERO);
                let m1 = self.b.ins().icmp_imm_s(IntCC::Equal, c, -1);
                let min = self.b.ins().icmp_imm_s(IntCC::Equal, a, i64::MIN);
                let both = self.b.ins().band(m1, min);
                self.bail_if(both, TRAP_OVERFLOW);
                self.b.ins().sdiv(a, c)
            }
            MirBinOp::Rem => {
                let z = self.b.ins().icmp_imm_s(IntCC::Equal, c, 0);
                self.bail_if(z, TRAP_DIV_ZERO);
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
                self.bail_if(out, TRAP_SHIFT);
                if op == MirBinOp::Shl { self.b.ins().ishl(a, c) } else { self.b.ins().sshr(a, c) }
            }
            MirBinOp::Pow => {
                let slot = self.b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    8,
                    3,
                ));
                let out = self.b.ins().stack_addr(types::I64, slot, 0);
                let call = self.b.ins().call(self.ipow, &[a, c, out]);
                let code = self.b.inst_results(call)[0];
                let neg = self.b.ins().icmp_imm_s(IntCC::Equal, code, machine::native::TRAP_NEGATIVE_EXPONENT as i64);
                self.bail_if(neg, machine::native::TRAP_NEGATIVE_EXPONENT);
                self.bail_if(code, TRAP_OVERFLOW);
                self.b.ins().stack_load(types::I64, types::I64, slot, 0)
            }
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
            Terminator::Return { lo, hi } => {
                self.keep_roots(f);
                let x = match lo {
                    Some(v) => self.get(f, *v),
                    None => self.b.ins().iconst(types::I64, 0),
                };
                let x = self.coerce(x, self.ret);
                match (hi, self.ret_hi) {
                    (None, None) => {
                        self.b.ins().return_(&[x]);
                    }
                    (Some(h), Some(t)) => {
                        let y = self.get(f, *h);
                        let y = self.coerce(y, t);
                        self.b.ins().return_(&[x, y]);
                    }
                    _ => return Err("return width differs from the signature".into()),
                }
            }
            Terminator::Unreachable => {
                let args = self.bail_args(TRAP_HELPER);
                self.b.ins().jump(self.bail, &args);
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
                        // binds is left to the runtime.
                        let call = self.b.ins().call(self.enum_tag, &[s]);
                        let tl = self.b.inst_results(call)[0];
                        let not_enum = self.b.ins().icmp_imm_s(IntCC::Equal, tl, -1);
                        self.bail_if(not_enum, TRAP_HELPER);
                        let low = self.b.ins().band_imm_u(tl, 0xffff_ffff);
                        let hit = self.b.ins().icmp_imm_s(IntCC::Equal, low, i64::from(*tag));
                        let check = self.b.create_block();
                        self.b.ins().brif(hit, check, &[], en, &[]);
                        self.b.switch_to_block(check);
                        let n = self.b.ins().ushr_imm_u(tl, 32);
                        let other = self.b.ins().icmp_imm_s(IntCC::NotEqual, n, payloads.len() as i64);
                        self.bail_if(other, TRAP_HELPER);
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
        }
        Ok(())
    }
}

/// Body names compiled per call to [`jit`], for `COIL_NATIVE_STATS`.
pub fn describe(p: &NativeProgram) -> String {
    let refused: HashSet<&str> = p.refused.iter().map(|(n, _)| n.as_str()).collect();
    format!(
        "native: {} compiled, {} installed, {} refused",
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
        let mut ctx = NativeCtx::new((std::ptr::addr_of!(marker) as u64).saturating_sub(NATIVE_STACK_BUDGET));
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
