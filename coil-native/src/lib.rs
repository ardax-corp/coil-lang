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

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::mem::ManuallyDrop;
use std::sync::Arc;

use compiler::native_mir::{
    AllocCall, BlockId, Label, MirAllocKind, alloc_call, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirFunc, MirInst, MirTy, MirUnaryOp,
    NativeBody, Terminator, ValueId, allocates, barrier_pairs, heap_word, is_generator, numeric, scalar,
    ty_of, root_handles, used_params, word_kind, in_range_steps,
};
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::ir::{AbiParam, AtomicRmwOp, Block, FuncRef, InstBuilder, MemFlagsData, Signature, Type, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use machine::native::{
    NATIVE_CTX_BAIL, NATIVE_CTX_HEAP, NATIVE_CTX_RET_HI, NATIVE_CTX_GC_DUE, NATIVE_CTX_GC_EPOCH, NATIVE_CTX_HAS_FINALIZERS, NATIVE_CTX_STATICS, NATIVE_CTX_STATICS_LEN, NATIVE_CTX_STACK_LIMIT, NATIVE_CTX_CALLS,
    NATIVE_CTX_GEN_HOMES, NATIVE_CTX_GEN_MASK,
    NativeEntry, NativeEntryFn, NativeSite, NO_SITE, TRAP_DIV_ZERO, TRAP_HELPER, TRAP_OVERFLOW, TRAP_SHIFT, TRAP_STACK,
};

/// `NativeCtx::gc_epoch` before a call in which code may run
/// ([`Lower::epoch_mark`]).
struct Epoch(cranelift_codegen::ir::Value);

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
    /// What `entries` point into. It goes with the last handle: hold one for
    /// as long as anything can run them (`NativeCode::with_keep`), or forget
    /// it to keep the code for the rest of the process.
    pub keep: Arc<dyn Any + Send + Sync>,
}

impl NativeProgram {
    /// Install every body for the coming run. Its entries point into the code
    /// for the rest of the process, so it stays.
    pub fn install(self) {
        std::mem::forget(self.keep);
        machine::native::install_for_next_run(self.entries, self.sites, true);
    }
}

/// A program's code in memory, freed with this. The module is only ever
/// dropped from here on, which is why it may cross threads.
struct Code {
    module: ManuallyDrop<JITModule>,
}

// SAFETY: nothing reads the module after it is built; `drop` has it alone.
unsafe impl Send for Code {}
// SAFETY: as for `Send`: a shared `Code` gives no access to the module.
unsafe impl Sync for Code {}

impl Code {
    fn keep(module: JITModule) -> Arc<dyn Any + Send + Sync> {
        Arc::new(Self { module: ManuallyDrop::new(module) })
    }
}

impl Drop for Code {
    fn drop(&mut self) {
        // SAFETY: this was the last handle, so no runtime can enter the code,
        // and `module` is not used again.
        unsafe { ManuallyDrop::take(&mut self.module).free_memory() };
    }
}

/// Compile every eligible body of `bodies` in memory. `body_ids` maps a
/// body's name to its entry id. The code stays mapped for as long as
/// [`NativeProgram::keep`].
///
/// Under `COIL_CODE_IMAGE_CHECK` the program goes through a code image and
/// the run uses the copy, as a code cache's next run would.
pub fn jit(bodies: &[NativeBody], body_ids: &HashMap<String, u32>) -> Result<NativeProgram, String> {
    if std::env::var_os("COIL_CODE_IMAGE_CHECK").is_some() {
        let program = jit_keeping_image(bodies, body_ids)?;
        return match &program.image {
            Some(image) => image::load(image),
            // A refused body leaves native code incomplete, so no image.
            None if !program.refused.is_empty() => Ok(program),
            None => Err("the code did not fit a code image".to_string()),
        };
    }
    jit_with(bodies, body_ids, false)
}

/// Bodies `COIL_NATIVE_REFUSE` (comma-separated names) leaves out, so a
/// test can reach what a refusal does.
fn forced_refusals(bodies: &[NativeBody]) -> HashMap<usize, String> {
    let Some(names) = std::env::var_os("COIL_NATIVE_REFUSE") else {
        return HashMap::new();
    };
    let names = names.to_string_lossy().into_owned();
    let names: HashSet<&str> = names.split(',').map(str::trim).collect();
    bodies
        .iter()
        .enumerate()
        .filter(|(_, b)| names.contains(b.name.as_str()))
        .map(|(i, _)| (i, "refused by COIL_NATIVE_REFUSE".to_string()))
        .collect()
}

/// [`jit`], with [`NativeProgram::image`] for [`load_image`] when every body
/// compiled and the code fits an image.
pub fn jit_keeping_image(bodies: &[NativeBody], body_ids: &HashMap<String, u32>) -> Result<NativeProgram, String> {
    jit_with(bodies, body_ids, true)
}

fn jit_with(bodies: &[NativeBody], body_ids: &HashMap<String, u32>, keep_image: bool) -> Result<NativeProgram, String> {
    // A body `plan` admits may still fail to lower; its callers already
    // name it, so leave it out and build the module again.
    let mut excluded = forced_refusals(bodies);
    loop {
        match jit_once(bodies, body_ids, &excluded, keep_image)? {
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
    program.install();
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
        MirAllocKind::Enum { tag } => Some(InlineShape::Enum { tag, kinds: (extra >> 32) as u32, n }),
        MirAllocKind::Object { type_id, nfields } => {
            Some(InlineShape::Instance { type_id, kinds: (extra >> 32) as u32, n: u8::try_from(nfields).ok()? })
        }
        _ => None,
    }
}

/// What [`aot`] produced: a relocatable object for the host target that
/// defines the compiled bodies and the tables `machine::native::install_aot`
/// reads (`coil_aot_entries`, `coil_aot_entry_count`, `coil_aot_sites`,
/// `coil_aot_site_count`, `coil_aot_refused_count`, `coil_aot_layout`), importing the runtime helpers by name, and the
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
/// the object imports each symbol and records its address. `all_linked`:
/// they are every `extern` function the program declares. `package` is
/// the program's data (`common::linked_package`), as `coil_aot_package`.
pub fn aot(bodies: &[NativeBody], body_ids: &HashMap<String, u32>, statics: &[(String, String)], all_linked: bool, package: &[u8]) -> Result<AotObject, String> {
    let mut excluded = HashMap::new();
    loop {
        let plan = plan_without(bodies, &excluded, body_ids);
        let refused = plan.refused.clone();
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
        flags.set("enable_verifier", VERIFY_CLIF).map_err(|e| e.to_string())?;
        // Executables are position independent on most hosts.
        flags.set("is_pic", "true").map_err(|e| e.to_string())?;
        flags.set("preserve_frame_pointers", "true").map_err(|e| e.to_string())?;
        stack_probes(&mut flags)?;
        let isa = cranelift_native::builder()
            .map_err(|e| e.to_string())?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())?;
        let compile_isa = isa.clone();
        let builder = cranelift_object::ObjectBuilder::new(isa, "coil_aot", default_libcall_names()).map_err(|e| e.to_string())?;
        let mut module = cranelift_object::ObjectModule::new(builder);
        let (ids, defined, sites) = match define_all(&mut module, bodies, body_ids, &plan, &*compile_isa, None)? {
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
            let Some(&pc) = body_ids.get(&body.name) else { continue };
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
            let twin = twin_of(bodies, body_ids, &plan, i);
            let off = std::mem::offset_of!(machine::native::AotEntry, twin);
            bytes[at + off..at + off + 4].copy_from_slice(&twin.to_le_bytes());
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
        let mut site_words = Vec::with_capacity(sites.len() * 20 + 20);
        for site in &sites {
            for w in [site.body_id, site.loc.file, site.loc.start_byte, site.loc.end_byte, site.loc.scope] {
                site_words.extend_from_slice(&w.to_le_bytes());
            }
        }
        let site_count = sites.len() as u64;
        if site_words.is_empty() {
            site_words.resize(20, 0);
        }
        words("coil_aot_sites", site_words)?;
        words("coil_aot_site_count", site_count.to_le_bytes().to_vec())?;
        words("coil_aot_refused_count", (refused.len() as u64).to_le_bytes().to_vec())?;
        words("coil_aot_layout", machine::native::layout_fingerprint().to_le_bytes().to_vec())?;
        static_symbol_table(&mut module, statics)?;
        host_group_table(&mut module, bodies)?;
        ffi_link_table(&mut module, bodies, all_linked && !ids.runtime_ffi.get())?;
        let bytes = module.finish().emit().map_err(|e| e.to_string())?;
        return Ok(AotObject { bytes, compiled, refused });
    }
}

/// The name of an AOT executable's native start ([`start_body`]).
pub const START_NAME: &str = common::NATIVE_START_NAME;

/// An AOT executable's start as a body (`__start`): the program's startup
/// in order (each `Init`'s word stored in its static, each `Call` made,
/// each `Restore` a `startup_restore` call), then `main`. Its payload then
/// is data only: the driver calls this body as the program's `main` and
/// walks no startup (`docs/internals/native.md`, "Native start").
/// `body_ids` names each body's id; `Err` names a step no body has.
pub fn start_body(bodies: &[NativeBody], body_ids: &HashMap<String, u32>, startup: &[common::StartupOp], main: Option<u32>) -> Result<NativeBody, String> {
    use compiler::native_mir::MirBlock;
    let by_id: HashMap<u32, &NativeBody> = bodies.iter().filter_map(|b| Some((*body_ids.get(&b.name)?, b))).collect();
    let mut f = MirFunc::new(START_NAME);
    f.ret_ty = Some(MirTy::I64);
    let mut b = MirBlock::new(BlockId(0));
    let val = |f: &mut MirFunc, ty: MirTy| {
        f.types.push(ty);
        ValueId(f.types.len() as u32 - 1)
    };
    let call = |f: &mut MirFunc, b: &mut MirBlock, id: u32| -> Result<ValueId, String> {
        let body = by_id.get(&id).ok_or_else(|| format!("no body {id} for the start"))?;
        let target = body.entry.ok_or_else(|| format!("`{}` has no entry label", body.name))?;
        let dest = val(f, body.mir.ret_ty.unwrap_or(MirTy::I64));
        let dest_hi = body.mir.ret_hi_ty.map(|t| val(f, t));
        let dest_more = body.mir.ret_more_tys.iter().map(|&t| val(f, t)).collect();
        b.insts.push(MirInst::Call { dest, dest_hi, dest_more, target, args: Vec::new() });
        Ok(dest)
    };
    for op in startup {
        match *op {
            common::StartupOp::Init { entry, slot } => {
                let value = call(&mut f, &mut b, entry)?;
                let dest = val(&mut f, MirTy::Bool);
                b.insts.push(MirInst::StoreStatic { dest, slot, value });
            }
            common::StartupOp::Call { entry } => {
                call(&mut f, &mut b, entry)?;
            }
            common::StartupOp::Restore { at } => {
                let c = val(&mut f, MirTy::I64);
                b.insts.push(MirInst::Const { dest: c, c: MirConst::I64(i64::from(at)) });
                let dest = val(&mut f, MirTy::I64);
                b.insts.push(MirInst::HostInvoke { dest, dest_hi: None, native_id: common::STARTUP_RESTORE_ID, layout: 0, args: vec![c] });
            }
        }
    }
    if let Some(m) = main {
        call(&mut f, &mut b, m)?;
    }
    let zero = val(&mut f, MirTy::I64);
    b.insts.push(MirInst::Const { dest: zero, c: MirConst::I64(0) });
    b.term = Some(Terminator::Return { lo: Some(zero), hi: None, more: Vec::new() });
    f.blocks = vec![b];
    Ok(NativeBody { name: START_NAME.to_string(), entry: None, mir: f })
}

/// `coil_aot_host_groups`: a `machine::AotHostGroup` record per host-native
/// group the program calls (`HostInvoke`) and every program's runtime
/// may call (`machine::core_host_groups`), its `push` the group's
/// `coil_host_group_K` symbol, and `coil_aot_host_group_count`. The link
/// keeps those groups' natives only.
fn host_group_table(module: &mut cranelift_object::ObjectModule, bodies: &[NativeBody]) -> Result<(), String> {
    let e = |e: cranelift_module::ModuleError| e.to_string();
    let groups = used_host_groups(bodies);
    const RECORD: usize = std::mem::size_of::<machine::AotHostGroup>();
    let spans = machine::host_group_spans();
    let mut bytes = vec![0u8; RECORD * groups.len().max(1)];
    for (i, &g) in groups.iter().enumerate() {
        let at = i * RECORD;
        let (first, count) = spans[g];
        let off = std::mem::offset_of!(machine::AotHostGroup, first);
        bytes[at + off..at + off + 4].copy_from_slice(&first.to_le_bytes());
        let off = std::mem::offset_of!(machine::AotHostGroup, count);
        bytes[at + off..at + off + 4].copy_from_slice(&count.to_le_bytes());
    }
    let mut table = cranelift_module::DataDescription::new();
    table.define(bytes.into_boxed_slice());
    table.set_align(8);
    for (i, &g) in groups.iter().enumerate() {
        let id = module.declare_data(&format!("coil_host_group_{g}"), Linkage::Import, false, false).map_err(e)?;
        let gv = module.declare_data_in_data(id, &mut table);
        table.write_data_addr((i * RECORD + std::mem::offset_of!(machine::AotHostGroup, push)) as u32, gv, 0);
    }
    let id = module.declare_data("coil_aot_host_groups", Linkage::Export, false, false).map_err(e)?;
    module.define_data(id, &table).map_err(e)?;
    let mut d = cranelift_module::DataDescription::new();
    d.define((groups.len() as u64).to_le_bytes().to_vec().into_boxed_slice());
    d.set_align(8);
    let id = module.declare_data("coil_aot_host_group_count", Linkage::Export, false, false).map_err(e)?;
    module.define_data(id, &d).map_err(e)
}

/// `coil_aot_ffi`: the address of the runtime's `coil_ffi_link` when
/// `bodies` make an FFI call (`MirInst::Ffi`), else null. A link without
/// the reference drops libffi, `dload` and its gate from the executable.
/// `direct`: every library the program loads is linked into the
/// executable and every extern call is direct, so with no
/// `MirFfiOp::Invoke` either it takes `coil_ffi_link_linked`, which has
/// none of them.
fn ffi_link_table(module: &mut cranelift_object::ObjectModule, bodies: &[NativeBody], direct: bool) -> Result<(), String> {
    let e = |e: cranelift_module::ModuleError| e.to_string();
    let mut d = cranelift_module::DataDescription::new();
    d.define(vec![0u8; 8].into_boxed_slice());
    d.set_align(8);
    if uses_ffi(bodies) {
        let invokes = bodies
            .iter()
            .flat_map(|b| b.mir.blocks.iter().flat_map(|b| &b.insts))
            .any(|i| matches!(i, MirInst::Ffi { op: compiler::native_mir::MirFfiOp::Invoke, .. }));
        let name = if direct && !invokes { "coil_ffi_link_linked" } else { "coil_ffi_link" };
        let link = module.declare_data(name, Linkage::Import, false, false).map_err(e)?;
        let gv = module.declare_data_in_data(link, &mut d);
        d.write_data_addr(0, gv, 0);
    }
    let id = module.declare_data("coil_aot_ffi", Linkage::Export, false, false).map_err(e)?;
    module.define_data(id, &d).map_err(e)
}

/// Whether `bodies` make an FFI call.
pub fn uses_ffi(bodies: &[NativeBody]) -> bool {
    bodies.iter().flat_map(|b| b.mir.blocks.iter().flat_map(|b| &b.insts)).any(|i| matches!(i, MirInst::Ffi { .. }))
}

/// The host-native groups (by number) `bodies` call, with the core ones, in order.
pub fn used_host_groups(bodies: &[NativeBody]) -> Vec<usize> {
    let mut groups: std::collections::BTreeSet<usize> = machine::core_host_groups().into_iter().collect();
    for inst in bodies.iter().flat_map(|b| b.mir.blocks.iter().flat_map(|b| &b.insts)) {
        if let MirInst::HostInvoke { native_id, .. } = inst
            && let Some(g) = machine::host_group_of(usize::from(*native_id))
        {
            groups.insert(g);
        }
    }
    groups.into_iter().collect()
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
    flags.set("enable_verifier", VERIFY_CLIF).map_err(|e| e.to_string())?;
    // Profilers and debuggers walk native frames by their frame pointers.
    flags.set("preserve_frame_pointers", "true").map_err(|e| e.to_string())?;
    stack_probes(&mut flags)?;
    let isa_builder = cranelift_native::builder().map_err(|e| e.to_string())?;
    isa_builder.finish(settings::Flags::new(flags)).map_err(|e| e.to_string())
}

/// Probe each page of a frame larger than one: Windows commits a thread's
/// stack a guard page at a time, so a frame that steps past the guard page
/// faults instead of growing the stack.
fn stack_probes(flags: &mut settings::Builder) -> Result<(), String> {
    flags.set("enable_probestack", "true").map_err(|e| e.to_string())?;
    flags.set("probestack_strategy", "inline").map_err(|e| e.to_string())
}

/// Cranelift's verifier: debug builds, which CI runs over every corpus. It
/// costs a release build a tenth of its compile.
const VERIFY_CLIF: &str = if cfg!(debug_assertions) { "true" } else { "false" };

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
    body_ids: &HashMap<String, u32>,
    excluded: &HashMap<usize, String>,
    keep_image: bool,
) -> Result<Attempt, String> {
    let plan = plan_without(bodies, excluded, body_ids);
    let refused = plan.refused.clone();
    if plan.compile.is_empty() {
        return Ok(Attempt::Done(NativeProgram { entries: Vec::new(), compiled: Vec::new(), refused, sites: Vec::new(), image: None, keep: Arc::new(()) }));
    }

    let reserve = jit_reserve(bodies, &plan);
    let mut module = jit_module(reserve)?;
    let isa = jit_isa()?;
    let mut recorder = keep_image.then(image::Recorder::default);

    let (ids, defined, sites) = match define_all(&mut module, bodies, body_ids, &plan, &*isa, recorder.as_mut())? {
        Ok(done) => done,
        Err((i, why)) => return Ok(Attempt::Without(i, why)),
    };
    module.finalize_definitions().map_err(|e| e.to_string())?;

    let mut entries = Vec::new();
    let mut entry_funcs = Vec::new();
    let mut compiled = Vec::new();
    for i in defined {
        let body = &bodies[i];
        compiled.push(body.name.clone());
        let Some(&pc) = body_ids.get(&body.name) else {
            continue;
        };
        let ptr = module.get_finalized_function(ids.entry[&i]);
        // SAFETY: `build_entry` gave this function the `NativeEntryFn` ABI.
        let entry: NativeEntryFn = unsafe { std::mem::transmute::<*const u8, NativeEntryFn>(ptr) };
        let ret_words = if body.mir.ret_hi_ty.is_some() { 2 } else { 1 };
        let reads = plan.used_params.get(&i).copied().unwrap_or(body.mir.params.len()) as u16;
        let generator = is_generator(&body.mir);
        let cleanup = body.mir.cleanup.is_some();
        let twin = twin_of(bodies, body_ids, &plan, i);
        let native = NativeEntry { pc, arity: body.mir.params.len() as u16, suspends: plan.runtime_reach.contains(&i), reads, ret_words, generator, cleanup, entry, data: 0, twin };
        entries.push(native);
        entry_funcs.push(image::Entry { func: ids.entry[&i], entry: native });
    }
    // A program with refused bodies does not run on native code alone.
    let image = recorder.filter(|_| refused.is_empty()).and_then(|recorder| {
        image::encode(image::Parts { module: &module, reserve, recorder, entries: &entry_funcs, sites: &sites, compiled: &compiled })
    });
    Ok(Attempt::Done(NativeProgram { entries, compiled, refused, sites, image, keep: Code::keep(module) }))
}

/// [`NativeEntry::twin`] of body `i`: one past its two-word twin's entry
/// id when the twin compiled and returns two words, else `0`.
fn twin_of(bodies: &[NativeBody], body_ids: &HashMap<String, u32>, plan: &Plan, i: usize) -> u32 {
    let Some(t) = bodies[i].mir.pair_twin.and_then(|l| plan.by_label.get(&l).copied()) else {
        return 0;
    };
    let twin = &bodies[t];
    if !plan.compile.contains(&t) || twin.mir.ret_hi_ty.is_none() || !twin.mir.ret_more_tys.is_empty() || twin.mir.params.len() != bodies[i].mir.params.len() {
        return 0;
    }
    body_ids.get(&twin.name).map_or(0, |&pc| pc + 1)
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
    /// Bodies a speculative auto-par job may run ([`speculative_reach`]):
    /// their loops that may run forever with no call poll the stack limit.
    speculative: HashSet<usize>,
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
    /// The bodies that may run while an object is pinned (`CowPin`,
    /// archive 8.2, [`pin_reach`]). The inline share of any other body leaves
    /// out its pinned path and the copy helper call that path makes.
    pins: HashSet<usize>,
    /// The program may make a C callback into coil ([`may_call_back`]):
    /// every extern call goes through the runtime, which roots the caller's
    /// words and reports a panic in the callback.
    ffi_callbacks: bool,
}

/// What [`define_all`] made: the declared ids, the bodies defined, the
/// and the panic sites their code names.
type Defined = (Ids, Vec<usize>, Vec<NativeSite>);

/// Declare and define every body `plan` compiles (its implementation,
/// cleanup pad and driver entry) in `module`. `Ok(Err((i, why)))` when body
/// `i` does not lower: the caller tries again without it.
#[allow(clippy::type_complexity)]
fn define_all<M: Module>(
    module: &mut M,
    bodies: &[NativeBody],
    body_ids: &HashMap<String, u32>,
    plan: &Plan,
    isa: &dyn cranelift_codegen::isa::TargetIsa,
    mut recorder: Option<&mut image::Recorder>,
) -> Result<Result<Defined, (usize, String)>, String> {
    let ids = declare(module, bodies, plan)?;
    if let Some(cb) = &ids.cb {
        define_callbacks(module, cb, recorder.as_deref_mut())?;
    }
    define_rc_last(module, &ids, recorder.as_deref_mut())?;
    let fcfg = module.isa().frontend_config();
    let mut fbc = FunctionBuilderContext::new();
    let mut ctx = module.make_context();
    let mut defined = Vec::new();
    let mut sites = Vec::new();
    // Lowering declares what it calls in the module, so it runs in order;
    // each function compiles on its own as soon as it is lowered.
    let (lowered, done) = compile_while(isa, compile_threads(plan.compile.len() * 2), |submit| {
        for &i in &plan.compile {
            let body = &bodies[i];
            ctx.func.signature = impl_signature(&*module, &body.mir, plan.tail_conv.contains(&i));
            let lowered = {
                let mut lower = Lower::new(module, &mut ctx.func, &mut fbc, &ids, plan, &mut sites, &body.mir);
                lower.body_id = common::site_body_id(body_ids, &body.name);
                lower.body_index = Some(i);
                lower.pins = plan.pins.contains(&i);
                let r = lower.body(&body.mir);
                if r.is_ok() {
                    lower.b.finalize(fcfg);
                }
                ids.runtime_ffi.set(ids.runtime_ffi.get() | lower.runtime_ffi);
                r
            };
            if let Err(why) = lowered {
                return Err((i, why));
            }
            submit(Job::take(ids.imp[&i], i, "", &mut ctx));
            module.clear_context(&mut ctx);

            if let (Some(pad), Some(&pad_id)) = (body.mir.cleanup.as_deref(), ids.pad.get(&i)) {
                ctx.func.signature = entry_signature(&*module);
                let lowered = {
                    // Its sites are its body's: a panic in a `defer` names it.
                    let mut lower = Lower::new(module, &mut ctx.func, &mut fbc, &ids, plan, &mut sites, pad);
                    lower.body_id = common::site_body_id(body_ids, &body.name);
                    lower.is_pad = true;
                    lower.pins = plan.pins.contains(&i);
                    let r = lower.body(pad);
                    if r.is_ok() {
                        lower.b.finalize(fcfg);
                    }
                    ids.runtime_ffi.set(ids.runtime_ffi.get() | lower.runtime_ffi);
                    r
                };
                if let Err(why) = lowered {
                    return Err((i, format!("cleanup pad: {why}")));
                }
                submit(Job::take(pad_id, i, " pad", &mut ctx));
                module.clear_context(&mut ctx);
            }

            ctx.func.signature = entry_signature(&*module);
            build_entry(module, &mut ctx.func, &mut fbc, &ids, ids.imp[&i], &body.mir);
            submit(Job::take(ids.entry[&i], i, " entry", &mut ctx));
            module.clear_context(&mut ctx);
            defined.push(i);
        }
        Ok(())
    });
    if let Err(failed) = lowered {
        return Ok(Err(failed));
    }
    for done in done {
        let func = match done {
            Ok(func) => func,
            Err(failed) => return Ok(Err(failed)),
        };
        module.define_function_bytes(func.id, func.align, &func.bytes, &func.relocs).map_err(|e| format!("cranelift: {e:?}"))?;
        if let Some(r) = recorder.as_deref_mut() {
            r.add(func);
        }
    }
    Ok(Ok((ids, defined, sites)))
}

/// A function lowered to Cranelift IR, waiting to compile.
struct Job {
    id: FuncId,
    func: cranelift_codegen::ir::Function,
    /// The body it belongs to.
    owner: usize,
    /// What it is of that body, for an error.
    part: &'static str,
}

impl Job {
    /// The function `ctx` holds, leaving it empty.
    fn take(id: FuncId, owner: usize, part: &'static str, ctx: &mut cranelift_codegen::Context) -> Self {
        let func = std::mem::replace(&mut ctx.func, cranelift_codegen::ir::Function::new());
        Self { id, func, owner, part }
    }
}

/// A compiled function; or the body that did not compile.
type Done = Result<image::Func, (usize, String)>;

fn compile_job(isa: &dyn cranelift_codegen::isa::TargetIsa, ctx: &mut cranelift_codegen::Context, job: Job) -> Done {
    ctx.clear();
    ctx.func = job.func;
    let mut plane = cranelift_codegen::control::ControlPlane::default();
    if let Err(e) = ctx.compile(isa, &mut plane) {
        return Err((job.owner, format!("cranelift{}: {:?}", job.part, e.inner)));
    }
    image::Func::of(job.id, ctx).ok_or((job.owner, "cranelift: no code".to_string()))
}

/// Functions a thread should have to itself before another is worth starting.
const JOBS_PER_THREAD: usize = 16;

/// A word view's state after an op that may have moved its array's storage:
/// not `0` (no view: not a word array) or [`machine::VIEW_PLAIN`]. The
/// next element access takes the view again.
const VIEW_UNKNOWN: i64 = -1;

/// Past this, lowering is what compiling waits for: more threads only contend.
const MAX_COMPILE_THREADS: usize = 8;

/// Threads to compile `jobs` functions with. `COIL_NATIVE_THREADS` sets the
/// most it may use; the output does not depend on it.
fn compile_threads(jobs: usize) -> usize {
    let most = std::env::var("COIL_NATIVE_THREADS").ok().and_then(|v| v.parse().ok());
    let most = most.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from).min(MAX_COMPILE_THREADS));
    (jobs / JOBS_PER_THREAD).clamp(1, most.max(1))
}

/// Run `produce`, compiling each job it submits: each is independent of the
/// rest, so `threads` work beside it. The results are in submission order, so
/// the code is the same on one thread or many.
fn compile_while<R>(isa: &dyn cranelift_codegen::isa::TargetIsa, threads: usize, produce: impl FnOnce(&mut dyn FnMut(Job)) -> R) -> (R, Vec<Done>) {
    use std::sync::{Mutex, PoisonError, mpsc};
    if threads == 1 {
        let mut ctx = cranelift_codegen::Context::new();
        let mut done = Vec::new();
        let out = produce(&mut |job| done.push(compile_job(isa, &mut ctx, job)));
        return (out, done);
    }
    let (tx, rx) = mpsc::channel::<(usize, Job)>();
    let rx = Mutex::new(rx);
    let finished: Mutex<Vec<(usize, Done)>> = Mutex::new(Vec::new());
    let work = || {
        let mut ctx = cranelift_codegen::Context::new();
        loop {
            // The lock is released before the job runs.
            let next = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
            let Ok((at, job)) = next else { return };
            let out = compile_job(isa, &mut ctx, job);
            finished.lock().unwrap_or_else(PoisonError::into_inner).push((at, out));
        }
    };
    let out = std::thread::scope(|scope| {
        let mut started = 0;
        for _ in 0..threads {
            // Deep bodies recurse in the compiler; match the main thread.
            if std::thread::Builder::new().name("coil-compile".into()).stack_size(8 << 20).spawn_scoped(scope, work).is_ok() {
                started += 1;
            }
        }
        let mut count = 0;
        let out = produce(&mut |job| {
            let _ = tx.send((count, job));
            count += 1;
        });
        drop(tx);
        // With no thread started, this one does the work.
        if started == 0 {
            work();
        }
        out
    });
    let mut done = finished.into_inner().unwrap_or_else(PoisonError::into_inner);
    done.sort_unstable_by_key(|(at, _)| *at);
    (out, done.into_iter().map(|(_, d)| d).collect())
}

/// [`plan`] with `excluded` bodies refused for the given reason.
fn plan_without(bodies: &[NativeBody], excluded: &HashMap<usize, String>, body_ids: &HashMap<String, u32>) -> Plan {
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
                        MirInst::ArrayPush { .. } | MirInst::LanePush { .. } | MirInst::LanesToWords { .. } | MirInst::HostInvoke { .. } | MirInst::CallIndirect { .. } | MirInst::ResumeCoro { .. } | MirInst::FreeEarly { .. } | MirInst::Yield { .. } | MirInst::Ffi { .. } | MirInst::ExternCall { .. } | MirInst::VecLoop { .. }
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
        .filter_map(|(&l, &i)| Some((l, *body_ids.get(&bodies[i].name)?)))
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
    // A body that returns more than two words: the platform's convention
    // returns two in registers, the `tail` one up to eight.
    tail_conv.extend(ok.keys().copied().filter(|&i| !bodies[i].mir.ret_more_tys.is_empty()));
    for (i, blocks) in &tail_sites {
        let f = &bodies[*i].mir;
        tail_conv.extend(f.blocks.iter().filter(|b| blocks.contains(&b.id)).filter_map(|b| match b.insts.last() {
            Some(MirInst::Call { target, .. }) => by_label.get(target).copied(),
            _ => None,
        }));
    }
    let speculative = speculative_reach(bodies, &by_label);
    let mut compile: Vec<usize> = ok.into_keys().collect();
    compile.sort_unstable();
    let pins = pin_reach(bodies, &by_label);
    let ffi_callbacks = may_call_back(bodies);
    Plan { compile, by_label, refused, moves, may_gc, speculative, pcs, drops, runtime_reach, used_params, tail_sites, tail_conv, pins, ffi_callbacks }
}

/// The labels `inst` names: a call's target, a function value's or code
/// pointer's code, a two-word call's `unpair`.
fn named_labels(inst: &MirInst) -> Vec<Label> {
    use compiler::native_mir::MirAllocKind as K;
    match inst {
        MirInst::Call { target, .. } | MirInst::CodePtr { target, .. } => vec![*target],
        MirInst::CallIndirect { pair, .. } => pair.iter().map(|&(_, l)| l).collect(),
        MirInst::Alloc { kind: K::Fn { target, .. } | K::PolyFn { target } | K::PolyFnCapture { target, .. } | K::Coro { target }, .. } => vec![*target],
        _ => Vec::new(),
    }
}

/// The bodies a speculative auto-par job may run (#2442): only those need
/// a poll in a loop that may run forever, since a cancel reaches a job
/// nowhere else. A job runs a function value a fork-join makes (the
/// bodies that call `thread_spawn_shared`, all compiler-made), and from
/// there whatever a body it runs names: its calls, the function values
/// and code pointers it makes. A body that calls a value or the host
/// (which may call one back) may reach any body whose code is taken
/// anywhere, and an allocation may run a type's `drop`.
fn speculative_reach(bodies: &[NativeBody], by_label: &HashMap<Label, usize>) -> HashSet<usize> {
    let insts = |i: usize| bodies[i].mir.blocks.iter().flat_map(|b| &b.insts);
    let mut work: Vec<usize> = Vec::new();
    for (i, _) in bodies.iter().enumerate() {
        if insts(i).any(|x| matches!(x, MirInst::HostInvoke { native_id, .. } if *native_id == common::THREAD_SPAWN_SHARED_ID)) {
            work.extend(insts(i).filter(|x| matches!(x, MirInst::Alloc { .. })).flat_map(named_labels).filter_map(|l| by_label.get(&l).copied()));
        }
    }
    let mut reach: HashSet<usize> = HashSet::new();
    let (mut values, mut drops) = (false, false);
    while let Some(i) = work.pop() {
        if !reach.insert(i) {
            continue;
        }
        work.extend(insts(i).flat_map(named_labels).filter_map(|l| by_label.get(&l).copied()));
        if !values && insts(i).any(|x| matches!(x, MirInst::CallIndirect { .. } | MirInst::HostInvoke { .. })) {
            values = true;
            for b in bodies {
                let taken = b.mir.blocks.iter().flat_map(|bb| &bb.insts).filter(|x| !matches!(x, MirInst::Call { .. }));
                work.extend(taken.flat_map(named_labels).filter_map(|l| by_label.get(&l).copied()));
            }
        }
        if !drops && insts(i).any(allocates) {
            drops = true;
            work.extend(bodies.iter().enumerate().filter(|(_, b)| b.name.ends_with("::drop")).map(|(j, _)| j));
        }
    }
    reach
}

/// The bodies that may run while some object is pinned: a share elsewhere
/// cannot meet a pinned object, so it needs no pin test (#2842). A pin is
/// set and cleared by one body (a `CowPin` and its `CowUnpin`), and the
/// object it pins is the one that body writes in place, made unique by the
/// caller, so while it is set only that body and what it runs can name the
/// object: its calls, the function values and code pointers it makes, any
/// body whose code is taken anywhere once it calls a value, the host, a
/// generator or foreign code (which may call one back), and the `drop`s an
/// allocation or a free may run. A pin a body leaves set at its exit is
/// never wrong to ignore: no in-place writer of it is live any more.
///
/// Every body, when what runs during a pin is not known from here: a body
/// in the reach that yields (its resumer runs with the pin set) or calls a
/// native that needs the runtime (which may park the task, so any task may
/// run), and any program that starts a thread (one may run any body at the
/// same time).
fn pin_reach(bodies: &[NativeBody], by_label: &HashMap<Label, usize>) -> HashSet<usize> {
    let insts = |i: usize| {
        let f = &bodies[i].mir;
        std::iter::once(f).chain(f.cleanup.as_deref()).flat_map(|f| f.blocks.iter().flat_map(|b| &b.insts))
    };
    let mut work: Vec<usize> = (0..bodies.len()).filter(|&i| insts(i).any(|x| matches!(x, MirInst::CowPin { .. }))).collect();
    if work.is_empty() {
        return HashSet::new();
    }
    let hosts = machine::native::direct_host_natives();
    let host = |id: u16| hosts.get(usize::from(id));
    let everything = || (0..bodies.len()).collect::<HashSet<usize>>();
    let threads = (0..bodies.len()).any(|i| {
        insts(i).any(|x| matches!(x, MirInst::HostInvoke { native_id, .. } if host(*native_id).is_some_and(|h| h.name.starts_with("thread_spawn"))))
    });
    if threads {
        return everything();
    }
    let mut reach: HashSet<usize> = HashSet::new();
    let (mut values, mut drops) = (false, false);
    while let Some(i) = work.pop() {
        if !reach.insert(i) {
            continue;
        }
        let unknown = insts(i).any(|x| match x {
            MirInst::Yield { .. } => true,
            MirInst::HostInvoke { native_id, .. } => host(*native_id).is_none_or(|h| h.collect || h.on_machine),
            _ => false,
        });
        if unknown {
            return everything();
        }
        work.extend(insts(i).flat_map(named_labels).filter_map(|l| by_label.get(&l).copied()));
        if !values && insts(i).any(|x| matches!(x, MirInst::CallIndirect { .. } | MirInst::HostInvoke { .. } | MirInst::ResumeCoro { .. } | MirInst::Ffi { .. } | MirInst::ExternCall { .. })) {
            values = true;
            for b in bodies {
                let f = &b.mir;
                let taken = std::iter::once(f).chain(f.cleanup.as_deref()).flat_map(|f| f.blocks.iter().flat_map(|bb| &bb.insts)).filter(|x| !matches!(x, MirInst::Call { .. }));
                work.extend(taken.flat_map(named_labels).filter_map(|l| by_label.get(&l).copied()));
            }
        }
        if !drops && insts(i).any(|x| allocates(x) || matches!(x, MirInst::FreeEarly { .. })) {
            drops = true;
            work.extend(bodies.iter().enumerate().filter(|(_, b)| b.name.ends_with("::drop")).map(|(j, _)| j));
        }
    }
    reach
}

/// Whether every view holds across a call to host native `id`: a leaf
/// ([`common::HostReach::Leaf`]) collects nothing and runs no code, and
/// one that changes no array it is handed (no `HEAP_MUT` effect, and not
/// `from_bytes_take`, which empties its buffer) moves no storage.
fn keeps_views(id: u16) -> bool {
    use common::EffectFlags as E;
    common::host_is_leaf(u64::from(id))
        && id != common::FROM_BYTES_TAKE_ID
        && common::HOST_NATIVES.get(usize::from(id)).is_some_and(|n| !n.effects.contains(E::HEAP_MUT))
}

/// An instruction that may run interpreted code: a function value's call, a free
/// (in a program with `drop`s) or collection that may run `drop`s.
fn reaches_runtime(inst: &MirInst, drops: bool) -> bool {
    match inst {
        MirInst::CallIndirect { .. } | MirInst::ResumeCoro { .. } | MirInst::Ffi { .. } | MirInst::ExternCall { .. } => true,
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
    let returns = |f: &MirFunc| (cl_ty(f.ret_ty.unwrap_or(MirTy::I64)), f.ret_hi_ty.map(cl_ty), f.ret_more_tys.iter().map(|&t| cl_ty(t)).collect::<Vec<_>>());
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
/// The arrays a word `VecLoop` reads (`false`) or stores (`true`) lanes
/// of, a push's output left out (it moves).
/// Whether `inst` may drop word views ([`Lower::drop_views`]) on its hot
/// path or in a collection it may run: anything but the ops known not to.
fn may_drop_views(inst: &MirInst) -> bool {
    !matches!(
        inst,
        MirInst::Const { .. }
            | MirInst::Bin { .. }
            | MirInst::Cmp { .. }
            | MirInst::Unary { .. }
            | MirInst::Cast { .. }
            | MirInst::Phi { .. }
            | MirInst::Index { .. }
            | MirInst::StoreIndex { .. }
            | MirInst::ArrayLen { .. }
            | MirInst::ProductLoad { .. }
            | MirInst::MatchPayload { .. }
            | MirInst::HeapFieldStore { .. }
            | MirInst::LaneIndex { .. }
            | MirInst::LaneStore { .. }
            | MirInst::HomeLoad { .. }
            | MirInst::HomeStore { .. }
            | MirInst::ClearWord { .. }
            | MirInst::KeepAlive { .. }
            // A count change frees only objects the frame no longer reads.
            | MirInst::RcInc { .. }
            | MirInst::RcDec { .. }
            | MirInst::RcReuse { .. }
    )
}

/// [`views_dropped_in`]'s step over `inst`: after an op that may drop
/// views, every viewed array; a phi takes its view at once; an element
/// access leaves its array's view taken.
fn drop_transfer(inst: &MirInst, viewed: &HashSet<ValueId>, set: &mut HashSet<ValueId>) {
    if may_drop_views(inst) {
        set.extend(viewed.iter().copied());
    }
    match inst {
        // A φ's word view comes from its sources, which may have dropped
        // theirs ([`Lower::edge_phis`]).
        MirInst::Phi { dest, .. } => {
            if viewed.contains(dest) {
                set.insert(*dest);
            }
        }
        MirInst::Index { array, .. } | MirInst::StoreIndex { array, .. } | MirInst::ArrayLen { array, .. } => {
            set.remove(array);
        }
        _ => {}
    }
}

/// For each block, the viewed arrays whose view some path into it may have
/// dropped since it was taken (a forward union to a fixed point). An access
/// to any other array reads through its view as it is, or through the
/// helpers when it is not one.
fn views_dropped_in(f: &MirFunc, viewed: &HashSet<ValueId>) -> HashMap<BlockId, HashSet<ValueId>> {
    let mut ins: HashMap<BlockId, HashSet<ValueId>> = HashMap::new();
    if viewed.is_empty() {
        return ins;
    }
    let mut changed = true;
    while changed {
        changed = false;
        for blk in &f.blocks {
            let mut set = ins.get(&blk.id).cloned().unwrap_or_default();
            for inst in &blk.insts {
                drop_transfer(inst, viewed, &mut set);
            }
            for s in blk.term.as_ref().map(|t| t.succs()).unwrap_or_default() {
                let into = ins.entry(s).or_default();
                let n = into.len();
                into.extend(set.iter().copied());
                changed |= into.len() != n;
            }
        }
    }
    ins
}

/// Every `(array, off, false)` load of `e`.
/// Lanes in one vector of a native lane loop (128-bit vectors of 64-bit
/// lanes), and lanes per iteration of its vector loop (four vectors).
const VEC_LANES: i64 = 2;
const VEC_BLOCK: i64 = 4 * VEC_LANES;
// A block's bytes are one 8-byte load (`vlane`).
const _: () = assert!(VEC_BLOCK == 8);

/// A native int add fold keeps a sum per lane while each term is in
/// `-2^REDUCE_ADD_TERM .. 2^REDUCE_ADD_TERM` and the loop's sum before the
/// lane sums is below `2^61` in size, for `REDUCE_ADD_BLOCKS` blocks at a
/// time: then every partial the scalar loop's left fold takes is below
/// `2^61 + 512 * 2^52 = 2^62`, so none overflows and the sum is the one it
/// gets. Past that the lane sums go into the loop's sum and start again.
const REDUCE_ADD_TERM: i64 = 52;
const REDUCE_ADD_BLOCKS: i64 = 512 / VEC_BLOCK;

/// Where a vector of a native lane loop's block sits: lanes `p`, `p + 1`,
/// vector `half` of the block from lane `p0`.
#[derive(Clone, Copy)]
struct VecAt {
    p0: cranelift_codegen::ir::Value,
    half: usize,
    p: cranelift_codegen::ir::Value,
}

/// What a native vector block found that it cannot take as it is: lanes
/// that would trap (a sign bit set in `vec`'s lanes, or `scal` non-zero).
/// The block then runs again one lane at a time.
#[derive(Default)]
struct VecBad {
    vec: Option<cranelift_codegen::ir::Value>,
    /// Lanes flagged when not zero (an `i64x2`).
    nz: Option<cranelift_codegen::ir::Value>,
    scal: Option<cranelift_codegen::ir::Value>,
}

impl VecBad {
    /// Flag the lanes of `of` (an `i64x2`) whose sign bit is set.
    fn vector(&mut self, g: &mut Lower<'_>, of: cranelift_codegen::ir::Value) {
        let of = g.vcoerce(of, types::I64X2);
        self.vec = Some(match self.vec {
            None => of,
            Some(v) => g.b.ins().bor(v, of),
        });
    }

    /// Flag the lanes of `x` (an `i64x2`) that are not zero.
    fn nonzero(&mut self, g: &mut Lower<'_>, x: cranelift_codegen::ir::Value) {
        self.nz = Some(match self.nz {
            None => x,
            Some(v) => g.b.ins().bor(v, x),
        });
    }

    /// Flag a lane when `flag` (an `i8`) is non-zero.
    fn scalar(&mut self, g: &mut Lower<'_>, flag: cranelift_codegen::ir::Value) {
        self.scal = Some(match self.scal {
            None => flag,
            Some(v) => g.b.ins().bor(v, flag),
        });
    }

    /// An `i8`, non-zero when any lane was flagged; `None` when none can be.
    fn any(&self, g: &mut Lower<'_>) -> Option<cranelift_codegen::ir::Value> {
        let v = self.vec.map(|v| g.b.ins().vhigh_bits(types::I8, v));
        let z = self.nz.map(|v| g.b.ins().vany_true(v));
        [v, z, self.scal].into_iter().flatten().reduce(|a, b| g.b.ins().bor(a, b))
    }
}

fn lane_refs(e: &compiler::native_mir::VecExpr, store: bool, out: &mut Vec<(ValueId, ValueId, bool)>) {
    use compiler::native_mir::VecExpr;
    match e {
        VecExpr::Splat { .. } | VecExpr::Index => {}
        VecExpr::Load { array, off, .. } => out.push((*array, *off, store)),
        VecExpr::Bin { lhs, rhs, .. } => {
            lane_refs(lhs, store, out);
            lane_refs(rhs, store, out);
        }
        VecExpr::Unary { src, .. } => lane_refs(src, store, out),
        VecExpr::Select { mask, then, els } => {
            lane_refs(mask, store, out);
            lane_refs(then, store, out);
            lane_refs(els, store, out);
        }
        VecExpr::Fma { a, b, c, .. } => {
            lane_refs(a, store, out);
            lane_refs(b, store, out);
            lane_refs(c, store, out);
        }
    }
}

fn vec_loop_arrays(kind: &compiler::native_mir::VecLoopKind, out: &mut Vec<(ValueId, bool)>) {
    use compiler::native_mir::{VecExpr, VecLoopKind};
    fn reads(e: &VecExpr, out: &mut Vec<(ValueId, bool)>) {
        match e {
            VecExpr::Splat { .. } | VecExpr::Index => {}
            VecExpr::Load { array, .. } => out.push((*array, false)),
            VecExpr::Bin { lhs, rhs, .. } => {
                reads(lhs, out);
                reads(rhs, out);
            }
            VecExpr::Unary { src, .. } => reads(src, out),
            VecExpr::Select { mask, then, els } => {
                reads(mask, out);
                reads(then, out);
                reads(els, out);
            }
            VecExpr::Fma { a, b, c, .. } => {
                reads(a, out);
                reads(b, out);
                reads(c, out);
            }
        }
    }
    match kind {
        VecLoopKind::Reduce { term, stores, .. } => {
            reads(term, out);
            for (array, _, _, value) in stores {
                reads(value, out);
                out.push((*array, true));
            }
        }
        VecLoopKind::Store { stores } => {
            for (array, _, _, value) in stores {
                reads(value, out);
                out.push((*array, true));
            }
        }
        VecLoopKind::Push { value, .. } => reads(value, out),
    }
}

/// Whether a lane value can differ from lane to lane: it reads an array
/// or the lane's index.
fn lane_varies(e: &compiler::native_mir::VecExpr) -> bool {
    use compiler::native_mir::VecExpr;
    match e {
        VecExpr::Splat { .. } => false,
        VecExpr::Index | VecExpr::Load { .. } => true,
        VecExpr::Bin { lhs, rhs, .. } => lane_varies(lhs) || lane_varies(rhs),
        VecExpr::Unary { src, .. } => lane_varies(src),
        VecExpr::Select { mask, then, els } => lane_varies(mask) || lane_varies(then) || lane_varies(els),
        VecExpr::Fma { a, b, c, .. } => lane_varies(a) || lane_varies(b) || lane_varies(c),
    }
}

fn value_used(f: &MirFunc, v: ValueId) -> bool {
    f.blocks.iter().any(|b| {
        b.insts.iter().any(|i| i.operands().contains(&v))
            || b.term.as_ref().is_some_and(|t| t.uses().any(|u| u == v) || t.binds().contains(&v))
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
    // Words 3.. of a wide return, beside `lo` and `hi`.
    if let Some(t) = f.ret_more_tys.iter().find(|t| !numeric(**t)) {
        return Err(format!("return type {t:?}"));
    }
    if f.ret_more_tys.len() > compiler::native_mir::MAX_CALL_RET_MORE || (!f.ret_more_tys.is_empty() && f.ret_hi_ty.is_none()) {
        return Err(format!("return of {} words past the second", f.ret_more_tys.len()));
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
                    MirInst::LaneIndex { dest, dest_hi, dest_more, array, index, .. } => {
                        need(dest)?;
                        need(dest_hi)?;
                        for w in dest_more {
                            need(w)?;
                        }
                        need_heap(array)?;
                        need_scalar(index)?;
                    }
                    MirInst::LaneStore { dest, array, index, lo, hi, more, .. } => {
                        need(dest)?;
                        need_heap(array)?;
                        need_scalar(index)?;
                        need(lo)?;
                        need(hi)?;
                        for w in more {
                            need(w)?;
                        }
                    }
                    MirInst::LanePush { dest, array, lo, hi, more, .. } => {
                        need(dest)?;
                        need_heap(array)?;
                        need(lo)?;
                        need(hi)?;
                        for w in more {
                            need(w)?;
                        }
                    }
                    MirInst::LanesToWords { dest, array, .. } => {
                        need(dest)?;
                        need_heap(array)?;
                    }
                    MirInst::ProductLoad { dest, object, .. } => {
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
                            MirAllocKind::Array | MirAllocKind::Lanes { .. } | MirAllocKind::Enum { .. } => {}
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
                    MirInst::CowIndex { dest, array, index } => {
                        need(dest)?;
                        need_heap(array)?;
                        need_scalar(index)?;
                    }
                    MirInst::CowPin { dest, src } => {
                        need_scalar(dest)?;
                        need(src)?;
                    }
                    MirInst::CowUnpin { dest, src, saved } => {
                        need(dest)?;
                        need(src)?;
                        need_scalar(saved)?;
                    }
                    MirInst::RcReuse { dest, src, .. } => {
                        need_scalar(dest)?;
                        need(src)?;
                    }
                    MirInst::AllocAt { dest, slot, kind, elems, .. } => {
                        need(dest)?;
                        if let Some(slot) = slot {
                            need_scalar(slot)?;
                        }
                        for e in elems {
                            need(e)?;
                        }
                        if !matches!(kind, MirAllocKind::Enum { .. } | MirAllocKind::Object { .. }) {
                            return Err("an allocation in place of a kind other than an enum or an instance".to_string());
                        }
                    }
                    MirInst::RcInc { dest, src, guard } | MirInst::RcDec { dest, src, guard } => {
                        need_scalar(dest)?;
                        need(src)?;
                        if let Some(g) = guard {
                            need_scalar(g)?;
                        }
                    }
                    MirInst::Phi { dest, args, .. } => {
                        need(dest)?;
                        for (_, v) in args {
                            need(v)?;
                        }
                    }
                    MirInst::Call { dest, dest_hi, dest_more, target, args } => {
                        if let Some(h) = dest_hi {
                            need(h)?;
                        }
                        for w in dest_more {
                            need(w)?;
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
                    MirInst::CallIndirect { dest, callee, args, pair, .. } => {
                        need(dest)?;
                        need(callee)?;
                        for a in args {
                            need(a)?;
                        }
                        // A two-word result reads the callee's one word
                        // through `unpair` when it has no twin.
                        if let Some((hi, unpair)) = pair {
                            need(hi)?;
                            let Some(&un) = by_label.get(unpair) else {
                                return Err("call to an unknown body".into());
                            };
                            callees.push(un);
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
                    MirInst::ExternCall { dest, sig, args } => {
                        need(dest)?;
                        for a in args {
                            need(a)?;
                        }
                        // The runtime's call takes all but the address.
                        if args.len() - 1 + sig.variadic.as_ref().map_or(0, Vec::len) > common::FFI_CALL_MAX_WORDS {
                            return Err("extern call of more words than the runtime's call takes".into());
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
                    MirInst::HostInvoke { dest, dest_hi, native_id, args, .. } => {
                        need(dest)?;
                        if let Some(h) = dest_hi {
                            need(h)?;
                        }
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
                }
                Ok(())
            };
            one().map_err(|e| if e.starts_with("instruction ") { e } else { format!("{e} in {}", inst_name(inst)) })?;
        }
        match &b.term {
            Some(Terminator::Jump { .. }) | Some(Terminator::Unreachable) => {}
            Some(Terminator::Br { cond, .. }) => need_scalar(cond)?,
            Some(Terminator::Return { lo, hi, more }) => {
                if lo.is_some() != f.ret_ty.is_some() || hi.is_some() != f.ret_hi_ty.is_some() || more.len() != f.ret_more_tys.len() {
                    return Err("return width".into());
                }
                for v in lo.iter().chain(hi).chain(more) {
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
    store_index: FuncId,
    set_field: FuncId,
    view: FuncId,
    lane_view: FuncId,
    str_view: FuncId,
    map_str_eq: FuncId,
    fn_entry: FuncId,
    fn_entry_pair: FuncId,
    /// Copy-on-write (these allocate, never collect).
    cow: FuncId,
    cow_field: FuncId,
    cow_index: FuncId,
    /// Reference count ops (`coil_native_rc`).
    rc: FuncId,
    /// A decrement's last owner ([`define_rc_last`]), generated per module.
    rc_last: FuncId,
    /// Reuse in place (`coil_native_rc_reuse`, `coil_native_rc_take`,
    /// `coil_native_offer_slot`).
    rc_reuse: FuncId,
    rc_take: FuncId,
    offer_slot: FuncId,
    /// Allocation (`coil_native_alloc`, `coil_native_array_push`,
    /// `coil_native_array_reserve`).
    alloc: FuncId,
    array_push: FuncId,
    array_reserve: FuncId,
    /// Lane array element ops (`LaneIndex` / `LaneStore` / `LanePush`)
    /// and the in-place `LanesToWords`.
    lane_index: FuncId,
    lane_store: FuncId,
    lane_push: FuncId,
    lanes_to_words: FuncId,
    /// The same of a wide element, its words through a stack buffer
    /// (`coil_native_lane_index_wide` / `_store_wide` / `_push_wide`).
    lane_index_wide: FuncId,
    lane_store_wide: FuncId,
    lane_push_wide: FuncId,
    probe: FuncId,
    /// Boxed enum matches.
    enum_tag: FuncId,
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
    ffi_arg: FuncId,
    ffi_done: FuncId,
    ffi_str: FuncId,
    ffi_struct_in: FuncId,
    ffi_struct_out: FuncId,
    va_call: FuncId,
    va_call_f: FuncId,
    /// A program whose C code may call back ([`Plan::ffi_callbacks`]): its
    /// direct calls' trampolines and its callbacks' thunks.
    cb: Option<CbIds>,
    /// Set when a lowered body calls an extern function through the
    /// runtime's libffi call ([`Lower::runtime_extern_call`]).
    runtime_ffi: std::cell::Cell<bool>,
}

/// [`CbIds`] a body uses, and its code pointers' body ids.
struct CbRefs {
    calls: HashMap<CShape, FuncRef>,
    thunks: HashMap<(u32, u32), FuncRef>,
    pcs: HashMap<ValueId, u32>,
}

/// [`Ids::cb`].
struct CbIds {
    /// A trampoline per [`CShape`]: `(ctx, addr, args..)`, which records its
    /// frame pointer and the call's context ([`machine::native::coil_native_cb_enter`])
    /// around the C call.
    calls: HashMap<CShape, FuncId>,
    /// A C-convention thunk per callback: the coil function's body id and
    /// the callback signature ([`machine::native::coil_native_callback`]).
    thunks: HashMap<(u32, u32), FuncId>,
    enter: FuncId,
    leave: FuncId,
    callback: FuncId,
}

/// Whether C code may call back into coil while an extern call runs: an
/// extern function takes a callback, or the program declares or invokes
/// through `ffi::declare` / `ffi::invoke`, whose signatures only the run
/// knows. A callback a library kept may run during any later call.
fn may_call_back(bodies: &[NativeBody]) -> bool {
    // Module setup declares each extern function: its callbacks are the
    // extern calls'.
    let setup = |b: &NativeBody| b.name.ends_with("__module_setup");
    bodies.iter().any(|b| {
        b.mir.blocks.iter().flat_map(|blk| &blk.insts).any(|i| match i {
            MirInst::ExternCall { sig, .. } => sig.params.iter().any(|&t| t & 0xFFFF == common::tag::CALLBACK),
            MirInst::Ffi { op: compiler::native_mir::MirFfiOp::Invoke, .. } => true,
            MirInst::Ffi { op: compiler::native_mir::MirFfiOp::Declare, .. } => !setup(b),
            _ => false,
        })
    })
}

/// How a direct extern call passes one argument.
#[derive(Clone, Copy)]
enum CArg {
    /// The word as this C type.
    Word(Type),
    /// A sized C integer (`int32`, `uint8`, …): the `int` word narrowed to
    /// this type and widened back (sign or zero extended, `signed`), passed
    /// as a whole register as C callers do.
    Int(Type, bool),
    /// The pointer [`machine::native::coil_native_ffi_arg`] makes of the word
    /// (`FFI_ARG_*`).
    Ptr(u64),
    /// A struct by value: struct `id`'s C bytes
    /// ([`machine::native::coil_native_ffi_struct_in`]) passed `how`, from a
    /// buffer of `size` bytes (a multiple of 8).
    Struct { id: u32, size: u32, how: SPass },
    /// A C function pointer: the native thunk that calls the coil function
    /// (its code pointer word) with this callback signature (a
    /// `tag::CALLBACK` aux).
    Callback(u32),
}

/// The C types of a direct extern call's signature as it is made: its
/// parameters (padding and a variadic call's address included) and
/// results. A call that a C callback may interrupt goes through one
/// trampoline per shape ([`define_callbacks`]).
type CShape = (Vec<Type>, Vec<Type>);

/// [`CShape`] of a direct call passing `params` and taking `ret` (`va` for
/// a variadic one with `nfixed` declared parameters), as
/// [`Lower::direct_extern_call`] makes it. `None` with a struct passed on
/// the stack or returned through a hidden pointer, special parameters a
/// trampoline does not forward.
fn c_shape(params: &[CArg], ret: CRet, va: Option<CVa>, nfixed: usize) -> Option<CShape> {
    let mut ps = Vec::new();
    for (i, p) in params.iter().enumerate() {
        if let (Some(CVa::Stack { gpad, fpad }), true) = (va, i == nfixed) {
            ps.extend(std::iter::repeat_n(types::I64, usize::from(gpad)));
            ps.extend(std::iter::repeat_n(types::F64, usize::from(fpad)));
        }
        ps.push(match *p {
            CArg::Word(t) => t,
            CArg::Int(..) | CArg::Ptr(_) | CArg::Callback(_) => types::I64,
            CArg::Struct { how: SPass::Regs(tys, n), .. } => {
                ps.extend_from_slice(&tys[..usize::from(n)]);
                continue;
            }
            CArg::Struct { how: SPass::Ref, .. } => types::I64,
            CArg::Struct { how: SPass::Stack, .. } => return None,
        });
    }
    match va {
        Some(CVa::Thunk { pad }) => ps.extend(std::iter::repeat_n(types::I64, usize::from(pad) + 1)),
        Some(CVa::ThunkF { pad }) => ps.extend(std::iter::repeat_n(types::F64, usize::from(pad) + 1)),
        _ => {}
    }
    let rs = match ret {
        CRet::Void => Vec::new(),
        CRet::Word(t) | CRet::Int(t, _) => vec![t],
        CRet::Bool => vec![types::I8],
        CRet::Str => vec![types::I64],
        CRet::Struct { n: 0, .. } => return None,
        CRet::Struct { regs, n, .. } => regs[..usize::from(n)].to_vec(),
    };
    Some((ps, rs))
}

/// A callback signature's C parameter and result types (`bool` a byte).
fn callback_c_types(aux: u32) -> Option<(Vec<Type>, Option<Type>)> {
    use common::tag;
    let (params, ret) = common::decode_callback_sig(aux)?;
    let ty = |t: u32| match t {
        tag::FLOAT => types::F64,
        tag::BOOL => types::I8,
        _ => types::I64,
    };
    Some((params.iter().map(|&t| ty(t)).collect(), (ret != tag::VOID).then(|| ty(ret))))
}

/// The code pointer each value of `f` holds that a `CodePtr` makes: the
/// body id of the function it names.
fn code_ptr_pcs(f: &MirFunc, plan: &Plan) -> HashMap<ValueId, u32> {
    f.blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|i| match i {
            MirInst::CodePtr { dest, target } => plan.pcs.get(target).map(|&pc| (*dest, pc)),
            _ => None,
        })
        .collect()
}

/// Where a struct argument's bytes go.
#[derive(Clone, Copy)]
enum SPass {
    /// In registers: its first `n` eightbytes, each loaded as that type.
    Regs([Type; 4], u8),
    /// Copied onto the stack by the call (x86-64 System V's memory class).
    Stack,
    /// A pointer to the caller's copy (AArch64 and Windows x64, when large).
    Ref,
}

/// What a direct extern call's C result is.
#[derive(Clone, Copy)]
enum CRet {
    Void,
    /// A word of this C type (`int`, `float`, `ptr`).
    Word(Type),
    /// A C `bool`: any nonzero byte is `true`.
    Bool,
    /// A sized C integer of this type, sign or zero extended (`signed`) to
    /// an `int`.
    Int(Type, bool),
    /// A C string, made a coil string ([`machine::native::coil_native_ffi_str`]).
    Str,
    /// A struct by value, made a record of struct `id`
    /// ([`machine::native::coil_native_ffi_struct_out`]): in registers (the
    /// eightbytes as these types), or (`n` `0`) written through a hidden
    /// pointer to a buffer of `size` bytes.
    Struct { id: u32, size: u32, regs: [Type; 4], n: u8 },
}

/// How a direct call to a variadic C function passes the arguments after
/// the declared ones (`...`), beyond what a fixed call does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CVa {
    /// As declared ones: AArch64's AAPCS64, and Windows x64 with `float`
    /// arguments in the integer registers (the `CArg`s say so).
    Plain,
    /// x86-64 System V: `al` holds an upper bound of the vector registers
    /// used. The call goes through [`machine::native::coil_native_va_call`],
    /// which sets it, with `pad` zero words and the C function's address
    /// after the arguments (so the address is in `r9`).
    Thunk { pad: u8 },
    /// x86-64 System V with every integer register taken by arguments: as
    /// `Thunk`, through [`machine::native::coil_native_va_call_f`], with
    /// `pad` zero `double`s and the address in `xmm7`.
    ThunkF { pad: u8 },
    /// Apple arm64: every variadic argument in an 8-byte stack slot. `gpad`
    /// integer and `fpad` floating-point zero words after the declared
    /// arguments use up the registers left.
    Stack { gpad: u8, fpad: u8 },
}

/// The C calling convention a direct extern call follows, as far as
/// passing structs by value and variadic arguments differs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CAbi {
    /// x86-64 System V (Linux, macOS, the BSDs).
    SysV64,
    /// Windows x64.
    Win64,
    /// AArch64 (AAPCS64, and Apple's variant of it, which passes structs
    /// the same way and variadic arguments on the stack).
    Arm64 { apple: bool },
    /// Anything else: struct calls go through the runtime.
    Other,
}

impl CAbi {
    fn of(isa: &dyn cranelift_codegen::isa::TargetIsa) -> Self {
        match (isa.name(), isa.default_call_conv()) {
            ("x64", CallConv::SystemV) => Self::SysV64,
            ("x64", CallConv::WindowsFastcall) => Self::Win64,
            ("aarch64", CallConv::SystemV) => Self::Arm64 { apple: false },
            ("aarch64", CallConv::AppleAarch64) => Self::Arm64 { apple: true },
            _ => Self::Other,
        }
    }

    /// Argument registers: general purpose and floating point.
    fn arg_regs(self) -> (usize, usize) {
        match self {
            Self::SysV64 => (6, 8),
            Self::Arm64 { .. } => (8, 8),
            Self::Win64 | Self::Other => (4, 4),
        }
    }
}

/// A struct's C shape for a call: its eightbytes' types when it travels in
/// registers (`float` eightbytes in floating-point ones), and whether it is
/// a homogeneous float aggregate (AArch64: one `double` register per
/// field). `None`: a field a direct call does not place (a string, a
/// pointer, a callback) or an empty struct.
fn c_struct_shape(st: &compiler::native_mir::MirCStruct) -> Option<(Vec<Type>, bool)> {
    use common::tag;
    if st.size == 0 || st.fields.is_empty() {
        return None;
    }
    let plain = |t: u32| matches!(t, tag::INT | tag::FLOAT | tag::BOOL | tag::INT8 | tag::INT16 | tag::INT32 | tag::UINT8 | tag::UINT16 | tag::UINT32 | tag::UINT64);
    if !st.fields.iter().all(|&(_, t)| plain(t)) {
        return None;
    }
    let words = st.size.div_ceil(8) as usize;
    let eightbytes = (0..words)
        .map(|w| {
            let lo = (w * 8) as u32;
            let all_float = st.fields.iter().filter(|&&(off, _)| off >= lo && off < lo + 8).all(|&(_, t)| t == tag::FLOAT);
            if all_float { types::F64 } else { types::I64 }
        })
        .collect();
    let hfa = st.fields.len() <= 4 && st.fields.iter().all(|&(_, t)| t == tag::FLOAT);
    Some((eightbytes, hfa))
}

/// A sized C integer tag's Cranelift type and whether it is signed.
fn sized_int(t: u32) -> (Type, bool) {
    use common::tag;
    match t {
        tag::INT8 => (types::I8, true),
        tag::INT16 => (types::I16, true),
        tag::INT32 => (types::I32, true),
        tag::UINT8 => (types::I8, false),
        tag::UINT16 => (types::I16, false),
        _ => (types::I32, false),
    }
}

fn regs_of(tys: &[Type]) -> [Type; 4] {
    let mut out = [types::I64; 4];
    out[..tys.len()].copy_from_slice(tys);
    out
}

/// How a direct call passes an extern call's arguments and takes its
/// result: `int`, the sized integers, `float`, `bool`, `string`, byte
/// views and buffers, `ptr` and (on x86-64 and AArch64) structs of numbers
/// and bools by value; the same results, but byte views, and `void`; a
/// variadic function's arguments after the declared ones (`...`) as C's
/// default promotions make them, but for structs, with the [`CVa`] the
/// convention needs. `None`: it has another struct or callback type, or
/// (x86-64 System V) a variadic call uses every integer and vector
/// register, and goes through the runtime.
fn direct_extern(sig: &compiler::native_mir::MirExternSig, abi: CAbi) -> Option<(Vec<CArg>, CRet, Option<CVa>)> {
    use common::tag;
    use machine::native::{FFI_ARG_BYTES, FFI_ARG_CONST_BYTES, FFI_ARG_CSTR, FFI_ARG_PTR};
    // A variadic call's tags name every argument; the declared ones first.
    let tail = match &sig.variadic {
        Some(tags) => Some(tags.get(sig.params.len()..)?),
        None => None,
    };
    let ret = match common::decode_tag_operand(sig.ret) {
        (tag::VOID, _) => CRet::Void,
        (tag::INT | tag::PTR | tag::UINT64, _) => CRet::Word(types::I64),
        (tag::FLOAT, _) => CRet::Word(types::F64),
        (t @ (tag::INT8 | tag::INT16 | tag::INT32 | tag::UINT8 | tag::UINT16 | tag::UINT32), _) => {
            let (ty, signed) = sized_int(t);
            CRet::Int(ty, signed)
        }
        (tag::BOOL, _) => CRet::Bool,
        (tag::STRING, _) => CRet::Str,
        (tag::STRUCT, id) => {
            let st = sig.struct_layout(id)?;
            let (words, hfa) = c_struct_shape(st)?;
            let size = st.size.next_multiple_of(8);
            let regs = match abi {
                CAbi::SysV64 if st.size <= 16 => words,
                CAbi::Arm64 { .. } if hfa => vec![types::F64; st.fields.len()],
                CAbi::Arm64 { .. } if st.size <= 16 => vec![types::I64; words.len()],
                CAbi::Win64 if matches!(st.size, 1 | 2 | 4 | 8) => vec![types::I64],
                // Any other size through a hidden pointer (`rcx` on Windows).
                CAbi::SysV64 | CAbi::Arm64 { .. } | CAbi::Win64 => Vec::new(),
                CAbi::Other => return None,
            };
            CRet::Struct { id, size, regs: regs_of(&regs), n: regs.len() as u8 }
        }
        _ => return None,
    };
    // The registers left as the arguments take them, for a struct that
    // must have all its eightbytes in registers or none.
    let (mut gprs, mut fprs) = abi.arg_regs();
    if matches!(ret, CRet::Struct { n: 0, .. }) && matches!(abi, CAbi::SysV64 | CAbi::Win64) {
        // The hidden result pointer (AArch64 has x8 for it).
        gprs -= 1;
    }
    let mut params = Vec::with_capacity(sig.params.len());
    for &t in &sig.params {
        let take = |gprs: &mut usize, fprs: &mut usize, ty: Type| {
            let r = if ty == types::F64 { fprs } else { gprs };
            *r = r.saturating_sub(1);
        };
        let p = match common::decode_tag_operand(t) {
            (tag::INT | tag::UINT64, _) => CArg::Word(types::I64),
            (tag::FLOAT, _) => CArg::Word(types::F64),
            (tag::BOOL, _) => CArg::Word(types::I8),
            (t @ (tag::INT8 | tag::INT16 | tag::INT32 | tag::UINT8 | tag::UINT16 | tag::UINT32), _) => {
                let (ty, signed) = sized_int(t);
                CArg::Int(ty, signed)
            }
            (tag::STRING, _) => CArg::Ptr(FFI_ARG_CSTR),
            (tag::CONST_BYTES, _) => CArg::Ptr(FFI_ARG_CONST_BYTES),
            (tag::BYTES, _) => CArg::Ptr(FFI_ARG_BYTES),
            (tag::PTR, _) => CArg::Ptr(FFI_ARG_PTR),
            (tag::CALLBACK, aux) => CArg::Callback(aux),
            (tag::STRUCT, id) => {
                let st = sig.struct_layout(id)?;
                let (words, hfa) = c_struct_shape(st)?;
                let size = st.size.next_multiple_of(8);
                let regs = |tys: &[Type], gprs: &mut usize, fprs: &mut usize| {
                    let ints = tys.iter().filter(|&&t| t != types::F64).count();
                    let floats = tys.len() - ints;
                    (ints <= *gprs && floats <= *fprs).then(|| {
                        *gprs -= ints;
                        *fprs -= floats;
                        SPass::Regs(regs_of(tys), tys.len() as u8)
                    })
                };
                let how = match abi {
                    // All of a small struct's eightbytes in registers, or
                    // the whole of it on the stack.
                    CAbi::SysV64 if st.size <= 16 => regs(&words, &mut gprs, &mut fprs).unwrap_or(SPass::Stack),
                    CAbi::SysV64 => SPass::Stack,
                    // One that does not fit in the registers left would
                    // go on the stack, which this path does not lay out.
                    CAbi::Arm64 { .. } if hfa => regs(&vec![types::F64; st.fields.len()], &mut gprs, &mut fprs)?,
                    CAbi::Arm64 { .. } if st.size <= 16 => regs(&vec![types::I64; words.len()], &mut gprs, &mut fprs)?,
                    CAbi::Win64 if matches!(st.size, 1 | 2 | 4 | 8) => SPass::Regs(regs_of(&[types::I64]), 1),
                    CAbi::Arm64 { .. } | CAbi::Win64 => {
                        gprs = gprs.saturating_sub(1);
                        SPass::Ref
                    }
                    CAbi::Other => return None,
                };
                params.push(CArg::Struct { id, size, how });
                continue;
            }
            _ => return None,
        };
        match p {
            CArg::Word(ty) => take(&mut gprs, &mut fprs, ty),
            CArg::Int(..) => take(&mut gprs, &mut fprs, types::I64),
            _ => take(&mut gprs, &mut fprs, types::I64),
        }
        params.push(p);
    }
    let Some(tail) = tail else {
        return Some((params, ret, None));
    };
    // Apple arm64: the registers the declared arguments leave are padded.
    let (gpad, fpad) = (gprs as u8, fprs as u8);
    for &t in tail {
        let p = match common::decode_tag_operand(t) {
            // A `bool` is promoted to `int`.
            (tag::INT | tag::BOOL, _) => CArg::Word(types::I64),
            // Windows x64 reads a variadic `double` from the integer
            // register of its position.
            (tag::FLOAT, _) if abi == CAbi::Win64 => CArg::Word(types::I64),
            (tag::FLOAT, _) => CArg::Word(types::F64),
            (tag::STRING, _) => CArg::Ptr(FFI_ARG_CSTR),
            (tag::CONST_BYTES, _) => CArg::Ptr(FFI_ARG_CONST_BYTES),
            (tag::BYTES, _) => CArg::Ptr(FFI_ARG_BYTES),
            (tag::PTR, _) => CArg::Ptr(FFI_ARG_PTR),
            _ => return None,
        };
        let ty = match p {
            CArg::Word(ty) => ty,
            _ => types::I64,
        };
        let r = if ty == types::F64 { &mut fprs } else { &mut gprs };
        *r = r.saturating_sub(1);
        params.push(p);
    }
    let va = match abi {
        // The C function's address takes an integer register of its own.
        CAbi::SysV64 if gprs >= 1 => CVa::Thunk { pad: gprs as u8 - 1 },
        // Otherwise the last vector register it has left.
        CAbi::SysV64 if fprs >= 1 => CVa::ThunkF { pad: fprs as u8 - 1 },
        CAbi::Arm64 { apple: true } => CVa::Stack { gpad, fpad },
        CAbi::Arm64 { apple: false } | CAbi::Win64 => CVa::Plain,
        CAbi::SysV64 | CAbi::Other => return None,
    };
    Some((params, ret, Some(va)))
}

/// A host call native code runs in place (`Lower::lower_map_op`).
#[derive(Clone, Copy, Debug)]
enum MapOp {
    /// `hash_string`: the string's cached hash.
    StrHash,
    /// `map_len`.
    Len,
    /// `map_live`.
    Live,
    /// `map_find` with keys compared as words, or as strings.
    Find { string: bool },
    /// `map_upsert`, likewise.
    Upsert { string: bool },
    /// `map_put`, keys likewise; `object` when the value word may name an
    /// object (counted as it is stored), `replace` when a found key's value
    /// is overwritten.
    Put { string: bool, object: bool, replace: bool },
}

/// The constant `v` holds, when a `Const` defines it.
fn const_i64(f: &MirFunc, v: ValueId) -> Option<i64> {
    f.blocks.iter().flat_map(|b| &b.insts).find_map(|i| match i {
        MirInst::Const { dest, c: MirConst::I64(k) } if *dest == v => Some(*k),
        _ => None,
    })
}

/// Which map table op host call `native_id(args)` is, when native code
/// runs it in place: a find or upsert only with its key mode a constant the
/// probe compares itself (word or string), and an upsert only where pushes
/// may be written in place (`ArraySlots::inline_push`).
fn map_op(f: &MirFunc, native_id: u16, args: &[ValueId]) -> Option<MapOp> {
    let mode = |at: usize| match args.get(at).and_then(|&m| const_i64(f, m)) {
        Some(common::KEY_EQ_WORD) => Some(false),
        Some(common::KEY_EQ_STRING) => Some(true),
        _ => None,
    };
    match (native_id, args.len()) {
        (common::HASH_STRING_ID, 1) => Some(MapOp::StrHash),
        (common::MAP_LEN_ID, 1) => Some(MapOp::Len),
        (common::MAP_LIVE_ID, 2) => Some(MapOp::Live),
        (common::MAP_FIND_ID, 5) => mode(4).map(|string| MapOp::Find { string }),
        (common::MAP_UPSERT_ID, 5) if machine::native::array_slots().inline_push => mode(4).map(|string| MapOp::Upsert { string }),
        (common::MAP_PUT_ID, 7) if machine::native::array_slots().inline_push => {
            let string = mode(4)?;
            let object = match const_i64(f, args[5])? {
                common::WORD_MODE_NUMBER => false,
                common::WORD_MODE_OBJECT => true,
                _ => return None,
            };
            let replace = match const_i64(f, args[6])? {
                0 => false,
                1 => true,
                _ => return None,
            };
            Some(MapOp::Put { string, object, replace })
        }
        _ => None,
    }
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
    for &t in &f.ret_more_tys {
        sig.returns.push(AbiParam::new(cl_ty(t)));
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

fn declare(module: &mut impl Module, bodies: &[NativeBody], plan: &Plan) -> Result<Ids, String> {
    let compile = &plan.compile;
    let e = |e: cranelift_module::ModuleError| e.to_string();
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
    let (s8, s7) = (words(8), words(7));
    let index = module.declare_function("coil_native_index", Linkage::Import, &s3).map_err(e)?;
    let len = module.declare_function("coil_native_len", Linkage::Import, &s1).map_err(e)?;
    let store_index = module.declare_function("coil_native_store_index", Linkage::Import, &s4).map_err(e)?;
    let set_field = module.declare_function("coil_native_set_field", Linkage::Import, &s4).map_err(e)?;
    let view = module.declare_function("coil_native_view", Linkage::Import, &s3).map_err(e)?;
    let lane_view = module.declare_function("coil_native_lane_view", Linkage::Import, &s4).map_err(e)?;
    let str_view = module.declare_function("coil_native_str_view", Linkage::Import, &s2).map_err(e)?;
    let map_str_eq = module.declare_function("coil_native_map_str_eq", Linkage::Import, &s2).map_err(e)?;
    let fn_entry = module.declare_function("coil_native_fn_entry", Linkage::Import, &s5).map_err(e)?;
    let fn_entry_pair = module.declare_function("coil_native_fn_entry_pair", Linkage::Import, &s5).map_err(e)?;
    let mut s1v = module.make_signature();
    s1v.params.push(AbiParam::new(types::I64));
    let cow = module.declare_function("coil_native_cow", Linkage::Import, &s3).map_err(e)?;
    let cow_field = module.declare_function("coil_native_cow_field", Linkage::Import, &s3).map_err(e)?;
    let cow_index = module.declare_function("coil_native_cow_index", Linkage::Import, &s3).map_err(e)?;
    let rc = module.declare_function("coil_native_rc", Linkage::Import, &s3).map_err(e)?;
    let rc_last = module.declare_function("coil_rc_last", Linkage::Local, &s2).map_err(e)?;
    let rc_reuse = module.declare_function("coil_native_rc_reuse", Linkage::Import, &s3).map_err(e)?;
    let rc_take = module.declare_function("coil_native_rc_take", Linkage::Import, &s3).map_err(e)?;
    let offer_slot = module.declare_function("coil_native_offer_slot", Linkage::Import, &s2).map_err(e)?;
    let alloc = module.declare_function("coil_native_alloc", Linkage::Import, &s5).map_err(e)?;
    let array_push = module.declare_function("coil_native_array_push", Linkage::Import, &s3).map_err(e)?;
    let array_reserve = module.declare_function("coil_native_array_reserve", Linkage::Import, &s3).map_err(e)?;
    let lane_index = module.declare_function("coil_native_lane_index", Linkage::Import, &s3).map_err(e)?;
    let lane_store = module.declare_function("coil_native_lane_store", Linkage::Import, &s8).map_err(e)?;
    let lane_push = module.declare_function("coil_native_lane_push", Linkage::Import, &s7).map_err(e)?;
    let lanes_to_words = module.declare_function("coil_native_lanes_to_words", Linkage::Import, &s5).map_err(e)?;
    let lane_index_wide = module.declare_function("coil_native_lane_index_wide", Linkage::Import, &s7).map_err(e)?;
    let lane_store_wide = module.declare_function("coil_native_lane_store_wide", Linkage::Import, &s8).map_err(e)?;
    let lane_push_wide = module.declare_function("coil_native_lane_push_wide", Linkage::Import, &s7).map_err(e)?;
    let probe = module.declare_function("coil_native_probe", Linkage::Import, &s2).map_err(e)?;
    let free_early = module.declare_function("coil_native_free_early", Linkage::Import, &s3).map_err(e)?;
    let format = module.declare_function("coil_native_format", Linkage::Import, &s4).map_err(e)?;
    let stringify = module.declare_function("coil_native_stringify", Linkage::Import, &s3).map_err(e)?;
    let eq = module.declare_function("coil_native_eq", Linkage::Import, &s3).map_err(e)?;
    let string = module.declare_function("coil_native_string", Linkage::Import, &s2).map_err(e)?;
    let enum_tag = module.declare_function("coil_native_enum_tag", Linkage::Import, &s1).map_err(e)?;
    let mut s3v = module.make_signature();
    s3v.params.extend((0..3).map(|_| AbiParam::new(types::I64)));
    let unwind = module.declare_function("coil_native_unwind", Linkage::Import, &s3v).map_err(e)?;
    let print = module.declare_function("coil_native_print", Linkage::Import, &s2).map_err(e)?;
    let host = module.declare_function("coil_native_host", Linkage::Import, &s5).map_err(e)?;
    let ffi = module.declare_function("coil_native_ffi", Linkage::Import, &s5).map_err(e)?;
    let mut s9 = module.make_signature();
    s9.params.extend((0..9).map(|_| AbiParam::new(types::I64)));
    s9.returns.push(AbiParam::new(types::I64));
    let vector = module.declare_function("coil_native_vector", Linkage::Import, &s9).map_err(e)?;
    let call_value = module.declare_function("coil_native_call_value", Linkage::Import, &s5).map_err(e)?;
    let resume = module.declare_function("coil_native_resume", Linkage::Import, &s4).map_err(e)?;
    let coro_done = module.declare_function("coil_native_coro_done", Linkage::Import, &s2).map_err(e)?;
    let gen_yield = module.declare_function("coil_native_yield", Linkage::Import, &s2).map_err(e)?;
    let mut s4v = module.make_signature();
    s4v.params.extend((0..4).map(|_| AbiParam::new(types::I64)));
    let defer = module.declare_function("coil_native_defer", Linkage::Import, &s4v).map_err(e)?;
    let claim = module.declare_function("coil_native_claim", Linkage::Import, &s3).map_err(e)?;
    let mut s1v2 = module.make_signature();
    s1v2.params.push(AbiParam::new(types::I64));
    let collect = module.declare_function("coil_native_collect", Linkage::Import, &s1v2).map_err(e)?;
    let panic = module.declare_function("coil_native_panic", Linkage::Import, &s2).map_err(e)?;
    let ffi_arg = module.declare_function("coil_native_ffi_arg", Linkage::Import, &s4).map_err(e)?;
    let ffi_done = module.declare_function("coil_native_ffi_done", Linkage::Import, &s1v2).map_err(e)?;
    let ffi_str = module.declare_function("coil_native_ffi_str", Linkage::Import, &s2).map_err(e)?;
    let ffi_struct_in = module.declare_function("coil_native_ffi_struct_in", Linkage::Import, &s4).map_err(e)?;
    let ffi_struct_out = module.declare_function("coil_native_ffi_struct_out", Linkage::Import, &s3).map_err(e)?;
    // Its signature is each call's own (it jumps to the C function).
    let va_call = module.declare_function("coil_native_va_call", Linkage::Import, &module.make_signature()).map_err(e)?;
    let va_call_f = module.declare_function("coil_native_va_call_f", Linkage::Import, &module.make_signature()).map_err(e)?;
    let cb = if plan.ffi_callbacks { Some(declare_callbacks(module, bodies, plan)?) } else { None };
    Ok(Ids {
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
        store_index,
        set_field,
        view,
        lane_view,
        str_view,
        map_str_eq,
        fn_entry,
        fn_entry_pair,
        cow,
        cow_field,
        cow_index,
        rc,
        rc_last,
        rc_reuse,
        rc_take,
        offer_slot,
        alloc,
        array_push,
        array_reserve,
        lane_index,
        lane_store,
        lane_push,
        lanes_to_words,
        lane_index_wide,
        lane_store_wide,
        lane_push_wide,
        probe,
        enum_tag,
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
        ffi_arg,
        ffi_done,
        ffi_str,
        ffi_struct_in,
        ffi_struct_out,
        va_call,
        va_call_f,
        cb,
        runtime_ffi: Default::default(),
    })
}

/// [`CbIds`] for the direct extern calls and callbacks of `plan`'s bodies.
fn declare_callbacks(module: &mut impl Module, bodies: &[NativeBody], plan: &Plan) -> Result<CbIds, String> {
    let e = |e: cranelift_module::ModuleError| e.to_string();
    let abi = CAbi::of(module.isa());
    let mut shapes = HashSet::new();
    let mut callbacks = HashSet::new();
    for &i in &plan.compile {
        let f = &bodies[i].mir;
        let pcs = code_ptr_pcs(f, plan);
        for inst in f.blocks.iter().flat_map(|b| &b.insts) {
            let MirInst::ExternCall { sig, args, .. } = inst else { continue };
            let Some((params, ret, va)) = direct_extern(sig, abi) else { continue };
            shapes.extend(c_shape(&params, ret, va, sig.params.len()));
            for (k, p) in params.iter().enumerate() {
                if let (CArg::Callback(aux), Some(&pc)) = (p, args.get(3 + k).and_then(|a| pcs.get(a))) {
                    callbacks.insert((pc, *aux));
                }
            }
        }
    }
    let mut shapes: Vec<CShape> = shapes.into_iter().collect();
    shapes.sort_by_key(|(p, r)| (p.iter().map(|t| t.to_string()).collect::<Vec<_>>(), r.iter().map(|t| t.to_string()).collect::<Vec<_>>()));
    let mut callbacks: Vec<(u32, u32)> = callbacks.into_iter().collect();
    callbacks.sort_unstable();
    let mut calls = HashMap::new();
    for (k, shape) in shapes.into_iter().enumerate() {
        let sig = cb_call_signature(module, &shape);
        calls.insert(shape, module.declare_function(&format!("coil_cb_call_{k}"), Linkage::Local, &sig).map_err(e)?);
    }
    let mut thunks = HashMap::new();
    for (pc, aux) in callbacks {
        let Some(sig) = callback_signature(module, aux) else { continue };
        thunks.insert((pc, aux), module.declare_function(&format!("coil_cb_thunk_{pc}_{aux}"), Linkage::Local, &sig).map_err(e)?);
    }
    let words = |n: usize| {
        let mut sig = module.make_signature();
        sig.params.extend((0..n).map(|_| AbiParam::new(types::I64)));
        sig.returns.push(AbiParam::new(types::I64));
        sig
    };
    let (s1, s2, s4) = (words(1), words(2), words(4));
    Ok(CbIds {
        calls,
        thunks,
        enter: module.declare_function("coil_native_cb_enter", Linkage::Import, &s1).map_err(e)?,
        leave: module.declare_function("coil_native_cb_leave", Linkage::Import, &s2).map_err(e)?,
        callback: module.declare_function("coil_native_callback", Linkage::Import, &s4).map_err(e)?,
    })
}

/// A [`CbIds::calls`] trampoline's signature: the context, the C
/// function's address and its arguments, its results.
fn cb_call_signature(module: &impl Module, (params, rets): &CShape) -> Signature {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.extend(params.iter().map(|&t| AbiParam::new(t)));
    sig.returns.extend(rets.iter().map(|&t| AbiParam::new(t)));
    sig
}

/// A callback thunk's C signature.
fn callback_signature(module: &impl Module, aux: u32) -> Option<Signature> {
    let (params, ret) = callback_c_types(aux)?;
    let mut sig = Signature::new(module.isa().default_call_conv());
    sig.params.extend(params.iter().map(|&t| AbiParam::new(t)));
    sig.returns.extend(ret.map(AbiParam::new));
    Some(sig)
}

/// Define the module's `coil_rc_last(ctx, word)`, which an inline
/// decrement calls when it reads the last owner of a typed object word with
/// no steal epoch open: a young string that no weak handle names, held
/// inline or as a view of a buffer some other string still holds, is freed
/// here, in generated code, when the heap's free gate is open
/// (`machine::native::string_free`): it drops its hold on the buffer, the
/// header is poisoned, the counters are taken down and the slot goes on the
/// string class's nursery free list. Anything else, and a view that held
/// its buffer's last reference, goes to the runtime (`coil_native_rc`, flag
/// three: a slot start, no slot search). Returns `0`.
fn define_rc_last<M: Module>(module: &mut M, ids: &Ids, recorder: Option<&mut image::Recorder>) -> Result<(), String> {
    use machine::native::{BUMP_BYTES, BUMP_FREE, BUMP_FREE_LINK, COUNTS_ALLOC_BYTES, COUNTS_LIVE_COUNT, NATIVE_CTX_ALLOC_BUMPS, NATIVE_CTX_ALLOC_COUNTS};
    let fcfg = module.isa().frontend_config();
    let mut fbc = FunctionBuilderContext::new();
    let mut ctx = module.make_context();
    let mut sig = module.make_signature();
    sig.params.extend([AbiParam::new(types::I64), AbiParam::new(types::I64)]);
    sig.returns.push(AbiParam::new(types::I64));
    ctx.func.signature = sig;
    let rc = module.declare_func_in_func(ids.rc, &mut ctx.func);
    let t = MemFlagsData::trusted();
    let sf = machine::native::string_free();
    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    let (gated, header, tagged, view_test, view, kept_buf, last_buf, free, call) =
        (b.create_block(), b.create_block(), b.create_block(), b.create_block(), b.create_block(), b.create_block(), b.create_block(), b.create_block(), b.create_block());
    b.append_block_param(free, types::I64);
    b.switch_to_block(entry);
    let (c_ctx, s) = (b.block_params(entry)[0], b.block_params(entry)[1]);
    let o = b.ins().band_imm_u(s, !1i64);
    // At zero already (a count the runtime keeps): the runtime's.
    let c = b.ins().uload32(t, o, machine::HEADER_COUNT_OFFSET);
    let one = b.ins().icmp_imm_s(IntCC::Equal, c, 1);
    b.ins().brif(one, gated, &[], call, &[]);
    b.switch_to_block(gated);
    let heap = b.ins().load(types::I64, t, c_ctx, NATIVE_CTX_HEAP);
    let gate = b.ins().uload8(types::I64, t, heap, machine::HEAP_FREE_GATE_OFFSET);
    b.ins().brif(gate, header, &[], call, &[]);
    b.switch_to_block(header);
    let h = b.ins().load(types::I64, t, o, 0);
    let bits = b.ins().band_imm_u(h, sf.header_mask as i64);
    let plain = b.ins().icmp_imm_s(IntCC::Equal, bits, sf.header_want as i64);
    b.ins().brif(plain, tagged, &[], call, &[]);
    b.switch_to_block(tagged);
    let tag = b.ins().uload8(types::I64, t, o, sf.tag_at as i32);
    let is_inline = b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(sf.inline_tag));
    let no_bytes = b.ins().iconst(types::I64, 0);
    b.ins().brif(is_inline, free, &[no_bytes.into()], view_test, &[]);
    b.switch_to_block(view_test);
    let is_view = b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(sf.shared_tag));
    b.ins().brif(is_view, view, &[], call, &[]);
    // A view lets go of its buffer as `SharedBuf`'s drop does; when it held
    // the last reference, it takes it back (no other holder can see it) and
    // the runtime frees both.
    b.switch_to_block(view);
    let buf = b.ins().load(types::I64, t, o, sf.buf_at as i32);
    let refs = b.ins().iadd_imm_s(buf, i64::from(sf.refs_at));
    let one = b.ins().iconst(types::I64, 1);
    let was = b.ins().atomic_rmw(types::I64, t, AtomicRmwOp::Sub, refs, one);
    let last = b.ins().icmp_imm_s(IntCC::Equal, was, 1);
    b.ins().brif(last, last_buf, &[], kept_buf, &[]);
    b.set_cold_block(last_buf);
    b.switch_to_block(last_buf);
    b.ins().atomic_rmw(types::I64, t, AtomicRmwOp::Add, refs, one);
    b.ins().jump(call, &[]);
    b.switch_to_block(kept_buf);
    let accounted = b.ins().uload32(t, o, sf.accounted_at as i32);
    b.ins().jump(free, &[accounted.into()]);
    b.switch_to_block(free);
    let accounted = b.block_params(free)[0];
    let kept = b.ins().band_imm_u(h, sf.header_keep as i64);
    b.ins().store(t, kept, o, 0);
    let counts = b.ins().load(types::I64, t, c_ctx, NATIVE_CTX_ALLOC_COUNTS);
    let bytes = b.ins().load(types::I64, t, counts, COUNTS_ALLOC_BYTES);
    let bytes = b.ins().isub(bytes, accounted);
    let bytes = b.ins().iadd_imm_s(bytes, -i64::from(sf.size));
    b.ins().store(t, bytes, counts, COUNTS_ALLOC_BYTES);
    let live = b.ins().load(types::I64, t, counts, COUNTS_LIVE_COUNT);
    let live = b.ins().iadd_imm_s(live, -1);
    b.ins().store(t, live, counts, COUNTS_LIVE_COUNT);
    let bumps = b.ins().load(types::I64, t, c_ctx, NATIVE_CTX_ALLOC_BUMPS);
    let cursor = b.ins().iadd_imm_s(bumps, i64::from(sf.class) * i64::from(BUMP_BYTES));
    let head = b.ins().load(types::I64, t, cursor, BUMP_FREE);
    b.ins().store(t, head, o, BUMP_FREE_LINK);
    b.ins().store(t, o, cursor, BUMP_FREE);
    let yes = b.ins().iconst(types::I8, 1);
    b.ins().store(t, yes, heap, machine::HEAP_FREED_EARLY_OFFSET);
    let zero = b.ins().iconst(types::I64, 0);
    b.ins().return_(&[zero]);
    b.set_cold_block(call);
    b.switch_to_block(call);
    let typed = b.ins().iconst(types::I64, 3);
    b.ins().call(rc, &[c_ctx, s, typed]);
    let zero = b.ins().iconst(types::I64, 0);
    b.ins().return_(&[zero]);
    b.seal_all_blocks();
    b.finalize(fcfg);
    module.define_function(ids.rc_last, &mut ctx).map_err(|e| format!("cranelift rc_last: {e:?}"))?;
    if let (Some(r), Some(func)) = (recorder, image::Func::of(ids.rc_last, &ctx)) {
        r.add(func);
    }
    module.clear_context(&mut ctx);
    Ok(())
}

/// Define [`CbIds`]' trampolines and thunks.
fn define_callbacks<M: Module>(module: &mut M, cb: &CbIds, mut recorder: Option<&mut image::Recorder>) -> Result<(), String> {
    let fcfg = module.isa().frontend_config();
    let conv = module.isa().default_call_conv();
    let mut fbc = FunctionBuilderContext::new();
    let mut ctx = module.make_context();
    let mut finish = |module: &mut M, id: FuncId, ctx: &mut cranelift_codegen::Context| -> Result<(), String> {
        module.define_function(id, ctx).map_err(|e| format!("cranelift callback: {e:?}"))?;
        if let (Some(r), Some(func)) = (recorder.as_deref_mut(), image::Func::of(id, ctx)) {
            r.add(func);
        }
        module.clear_context(ctx);
        Ok(())
    };
    let mut calls: Vec<_> = cb.calls.iter().collect();
    calls.sort_by_key(|(_, id)| id.as_u32());
    for (shape, &id) in calls {
        // saved = enter(ctx); r = addr(args..);
        // leave(ctx, saved); return r
        ctx.func.signature = cb_call_signature(module, shape);
        let enter = module.declare_func_in_func(cb.enter, &mut ctx.func);
        let leave = module.declare_func_in_func(cb.leave, &mut ctx.func);
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
        let blk = b.create_block();
        b.append_block_params_for_function_params(blk);
        b.switch_to_block(blk);
        let params = b.block_params(blk).to_vec();
        let (c, addr) = (params[0], params[1]);
        let call = b.ins().call(enter, &[c]);
        let saved = b.inst_results(call)[0];
        let mut csig = Signature::new(conv);
        csig.params.extend(shape.0.iter().map(|&t| AbiParam::new(t)));
        csig.returns.extend(shape.1.iter().map(|&t| AbiParam::new(t)));
        let csig = b.import_signature(csig);
        let call = b.ins().call_indirect(csig, addr, &params[2..]);
        let rets = b.inst_results(call).to_vec();
        b.ins().call(leave, &[c, saved]);
        b.ins().return_(&rets);
        b.seal_all_blocks();
        b.finalize(fcfg);
        finish(module, id, &mut ctx)?;
    }
    let mut thunks: Vec<_> = cb.thunks.iter().collect();
    thunks.sort_by_key(|(_, id)| id.as_u32());
    for (&(pc, aux), &id) in thunks {
        // words = [args as words]; w = callback(pc, aux, words, n); return w as C's
        let Some(sig) = callback_signature(module, aux) else { continue };
        let ret = sig.returns.first().map(|p| p.value_type);
        ctx.func.signature = sig;
        let callback = module.declare_func_in_func(cb.callback, &mut ctx.func);
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
        let blk = b.create_block();
        b.append_block_params_for_function_params(blk);
        b.switch_to_block(blk);
        let params = b.block_params(blk).to_vec();
        let n = params.len();
        let slot = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(cranelift_codegen::ir::StackSlotKind::ExplicitSlot, (n.max(1) * 8) as u32, 3));
        for (k, &p) in params.iter().enumerate() {
            let w = match b.func.dfg.value_type(p) {
                types::F64 => b.ins().bitcast(types::I64, MemFlagsData::new(), p),
                types::I8 => {
                    let nz = b.ins().icmp_imm_u(IntCC::NotEqual, p, 0);
                    b.ins().uextend(types::I64, nz)
                }
                _ => p,
            };
            b.ins().stack_store(types::I64, w, slot, (k * 8) as i32);
        }
        let at = b.ins().stack_addr(types::I64, slot, 0);
        let pc = b.ins().iconst(types::I64, i64::from(pc));
        let aux = b.ins().iconst(types::I64, i64::from(aux));
        let n = b.ins().iconst(types::I64, n as i64);
        let call = b.ins().call(callback, &[pc, aux, at, n]);
        let w = b.inst_results(call)[0];
        match ret {
            None => {
                b.ins().return_(&[]);
            }
            Some(types::F64) => {
                let x = b.ins().bitcast(types::F64, MemFlagsData::new(), w);
                b.ins().return_(&[x]);
            }
            Some(types::I8) => {
                let x = b.ins().ireduce(types::I8, w);
                b.ins().return_(&[x]);
            }
            Some(_) => {
                b.ins().return_(&[w]);
            }
        }
        b.seal_all_blocks();
        b.finalize(fcfg);
        finish(module, id, &mut ctx)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The runtime entry wrapper

fn build_entry(
    module: &mut impl Module,
    func: &mut cranelift_codegen::ir::Function,
    fbc: &mut FunctionBuilderContext,
    ids: &Ids,
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
    // A body that returns more than two words is entered only by a direct
    // call: the runtime's entry has no place for its words 3.., so it
    // panics here rather than give the caller part of the result.
    if !f.ret_more_tys.is_empty() {
        let unwind = module.declare_func_in_func(ids.unwind, b.func);
        let code = b.ins().iconst(types::I64, TRAP_HELPER as i64);
        let site = b.ins().iconst(types::I64, NO_SITE as i64);
        b.ins().call(unwind, &[ctx, code, site]);
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().return_(&[zero]);
        b.seal_all_blocks();
        b.finalize(module.isa().frontend_config());
        return;
    }
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
    store_index: FuncRef,
    set_field: FuncRef,
    view: FuncRef,
    lane_view: FuncRef,
    str_view: FuncRef,
    /// `coil_native_map_str_eq`: the map probe's string key compare.
    map_str_eq: FuncRef,
    fn_entry: FuncRef,
    fn_entry_pair: FuncRef,
    /// [`entry_signature`], for a call of a body's entry by address.
    entry_sig: cranelift_codegen::ir::SigRef,
    /// The platform's C calling convention, for a direct extern call.
    c_conv: CallConv,
    /// How that convention passes structs by value.
    c_abi: CAbi,
    /// Extern calls may be direct: no C callback can run coil code
    /// ([`Plan::ffi_callbacks`]).
    direct_externs: bool,
    /// This body calls an extern function through the runtime.
    runtime_ffi: bool,
    cow: FuncRef,
    cow_field: FuncRef,
    cow_index: FuncRef,
    rc: FuncRef,
    rc_last: FuncRef,
    rc_reuse: FuncRef,
    rc_take: FuncRef,
    offer_slot: FuncRef,
    alloc: FuncRef,
    array_push: FuncRef,
    array_reserve: FuncRef,
    lane_index: FuncRef,
    lane_store: FuncRef,
    lane_push: FuncRef,
    lanes_to_words: FuncRef,
    lane_index_wide: FuncRef,
    lane_store_wide: FuncRef,
    lane_push_wide: FuncRef,
    probe: FuncRef,
    enum_tag: FuncRef,
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
    ffi_arg: FuncRef,
    ffi_done: FuncRef,
    ffi_str: FuncRef,
    ffi_struct_in: FuncRef,
    ffi_struct_out: FuncRef,
    va_call: FuncRef,
    va_call_f: FuncRef,
    /// The body's direct extern calls run in a program whose C code may
    /// call back: through a [`CbIds::calls`] trampoline.
    cb: Option<CbRefs>,
    /// This body's index into the plan's bodies.
    body_index: Option<usize>,
    /// The body may run while an object is pinned ([`pin_reach`]): its
    /// inline share tests the pin.
    pins: bool,
    /// Lowering a cleanup pad: its homes are behind its second parameter.
    is_pad: bool,
    /// Inside a native lane loop whose every element access was checked in
    /// front of it ([`Lower::lanes_guard`]): loads read in place unchecked.
    lanes_checked: bool,
    /// In a native lane loop over bytes: each byte source's first byte
    /// (its loads read bytes at `base + index`, [`Self::lane`], [`Self::vlane`]).
    lane_bytes: HashMap<ValueId, cranelift_codegen::ir::Value>,
    /// A vector block's byte loads, widened once per block: `(array, off)`
    /// to its vectors.
    vec_bytes: HashMap<(ValueId, ValueId), Vec<cranelift_codegen::ir::Value>>,
    /// The arrays whose word view an op before this one may have dropped
    /// ([`views_dropped_in`]): only their element accesses take a dropped
    /// view again.
    maybe_dropped: HashSet<ValueId>,
    /// Where the homes live: this frame's slot (`[next, n, words...]` is
    /// not needed: they are copied out at a panic), or the pad's pointer.
    homes_slot: Option<cranelift_codegen::ir::StackSlot>,
    homes_ptr: Option<cranelift_codegen::ir::Value>,
    /// The [`Self::push_call`] record of this frame.
    calls_slot: Option<cranelift_codegen::ir::StackSlot>,
    /// `gc::root` handles the body makes: kept alive until it returns
    /// ([`Self::keep_roots`]), where other words die at their last use.
    root_handles: Vec<ValueId>,
    /// Where [`Self::keep_roots`] stores them (made on first use).
    keep_slot: Option<cranelift_codegen::ir::StackSlot>,
    /// `int` steps a dominating compare keeps in range
    /// ([`in_range_steps`]): a plain add, with no overflow check.
    in_range: HashSet<ValueId>,
    defer: FuncRef,
    claim: FuncRef,
    /// This body's cleanup pad.
    pad_fn: Option<FuncRef>,
    pad_refs: HashMap<usize, FuncRef>,
    /// Panic sites of every body so far; this body's ops name theirs by
    /// index ([`Self::site`]).
    sites: &'a mut Vec<NativeSite>,
    /// This body's entry id, which its sites name (`None`: no sites).
    body_id: Option<u32>,
    /// The location of the op being lowered, and its site once a bail
    /// path named it.
    cur_loc: Option<common::DebugLoc>,
    cur_site: Option<i64>,
    /// Arrays this body indexes: `(ok, pointer, length)` of their element
    /// words in place, set where the array value is defined (see
    /// `machine::native::coil_native_view`).
    views: HashMap<ValueId, (Variable, Variable, Variable)>,
    /// Strings this body reads `s[i]` of: `(known, pointer, length)` of
    /// their bytes, filled at the first read after the string value is
    /// defined or the views were dropped (`machine::native::coil_native_str_view`;
    /// length 0 when the word is not a string object itself).
    str_views: HashMap<ValueId, (Variable, Variable, Variable)>,
    /// Enums this body reads payload words of: `(pointer, length)` of
    /// their payload words, taken at the first tag test or payload read
    /// after the value is defined (length 0 until then), so the other
    /// reads of the same match skip the layout tests. An enum's payload
    /// is never written once made; a value the body also reads or writes
    /// as a class's fields is left out.
    payload_views: HashMap<ValueId, (Variable, Variable)>,
    /// Objects this body reads or writes fields of in two or more blocks:
    /// `(pointer, length)` of their slot words, taken at the first field
    /// access after the value is defined (length 0 until then), so the
    /// other accesses skip the kind and storage tests. An object's slot
    /// storage is fixed when it is made: field writes change the words,
    /// never where they are.
    product_views: HashMap<ValueId, (Variable, Variable)>,
    /// The `(pointer, length)` of the slot words of the objects whose fields
    /// the current block reached so far, for its later accesses of the
    /// same object ([`Self::product_views`] within one block, with no
    /// variable to carry along the edges).
    block_views: HashMap<ValueId, (cranelift_codegen::ir::Value, cranelift_codegen::ir::Value)>,
    /// Lane arrays this body reads or writes elements of in place:
    /// `(state, pointer, element count)` of their element words
    /// (`machine::native::coil_native_lane_view`). `state` is 1 for a view
    /// in lanes, 2 for an array of one word per element (the lane helpers
    /// box and unbox), 0 not known: filled where the array value is
    /// defined, or at the next element op after the views were dropped.
    lane_views: HashMap<ValueId, (Variable, Variable, Variable)>,
    /// The words per element of each lane view's array (2 when not
    /// listed; a wide element's 3 or 4).
    lane_words: HashMap<ValueId, u8>,
    /// Array values this body stores into.
    stored: HashSet<ValueId>,
    /// `GcBarrier` dest -> the object it names ([`barrier_pairs`]).
    barriers: HashMap<ValueId, ValueId>,
    /// The words `coil_native_view` (three) and the lane and string views
    /// (two) write to.
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
    /// The types of results 3.. of a wider return (`MirFunc::ret_more_tys`).
    ret_more: Vec<Type>,
}

impl<'a> Lower<'a> {
    fn new(
        module: &mut impl Module,
        func: &'a mut cranelift_codegen::ir::Function,
        fbc: &'a mut FunctionBuilderContext,
        ids: &Ids,
        plan: &'a Plan,
        sites: &'a mut Vec<NativeSite>,
        body: &MirFunc,
    ) -> Self {
        let by_label = &plan.by_label;
        // Only what `body` calls: every body declaring every other is
        // quadratic in the program.
        let mut module_refs = HashMap::new();
        for inst in body.blocks.iter().flat_map(|b| &b.insts) {
            if let MirInst::Call { target, .. } | MirInst::CallIndirect { pair: Some((_, target)), .. } = inst
                && let Some(&i) = by_label.get(target)
                && let Some(&id) = ids.imp.get(&i)
            {
                module_refs.entry(i).or_insert_with(|| module.declare_func_in_func(id, func));
            }
        }
        let fmod = module.declare_func_in_func(ids.fmod, func);
        let powf = module.declare_func_in_func(ids.powf, func);
        let ipow = module.declare_func_in_func(ids.ipow, func);
        let index = module.declare_func_in_func(ids.index, func);
        let len = module.declare_func_in_func(ids.len, func);
        let store_index = module.declare_func_in_func(ids.store_index, func);
        let set_field = module.declare_func_in_func(ids.set_field, func);
        let view = module.declare_func_in_func(ids.view, func);
        let lane_view = module.declare_func_in_func(ids.lane_view, func);
        let str_view = module.declare_func_in_func(ids.str_view, func);
        let map_str_eq = module.declare_func_in_func(ids.map_str_eq, func);
        let fn_entry = module.declare_func_in_func(ids.fn_entry, func);
        let fn_entry_pair = module.declare_func_in_func(ids.fn_entry_pair, func);
        let entry_sig = func.import_signature(entry_signature(module));
        let cow = module.declare_func_in_func(ids.cow, func);
        let cow_field = module.declare_func_in_func(ids.cow_field, func);
        let cow_index = module.declare_func_in_func(ids.cow_index, func);
        let rc = module.declare_func_in_func(ids.rc, func);
        let rc_last = module.declare_func_in_func(ids.rc_last, func);
        let rc_reuse = module.declare_func_in_func(ids.rc_reuse, func);
        let rc_take = module.declare_func_in_func(ids.rc_take, func);
        let offer_slot = module.declare_func_in_func(ids.offer_slot, func);
        let alloc = module.declare_func_in_func(ids.alloc, func);
        let array_push = module.declare_func_in_func(ids.array_push, func);
        let array_reserve = module.declare_func_in_func(ids.array_reserve, func);
        let lane_index = module.declare_func_in_func(ids.lane_index, func);
        let lane_store = module.declare_func_in_func(ids.lane_store, func);
        let lane_push = module.declare_func_in_func(ids.lane_push, func);
        let lanes_to_words = module.declare_func_in_func(ids.lanes_to_words, func);
        let lane_index_wide = module.declare_func_in_func(ids.lane_index_wide, func);
        let lane_store_wide = module.declare_func_in_func(ids.lane_store_wide, func);
        let lane_push_wide = module.declare_func_in_func(ids.lane_push_wide, func);
        let probe = module.declare_func_in_func(ids.probe, func);
        let enum_tag = module.declare_func_in_func(ids.enum_tag, func);
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
        let ffi_arg = module.declare_func_in_func(ids.ffi_arg, func);
        let ffi_done = module.declare_func_in_func(ids.ffi_done, func);
        let ffi_str = module.declare_func_in_func(ids.ffi_str, func);
        let ffi_struct_in = module.declare_func_in_func(ids.ffi_struct_in, func);
        let ffi_struct_out = module.declare_func_in_func(ids.ffi_struct_out, func);
        let va_call = module.declare_func_in_func(ids.va_call, func);
        let va_call_f = module.declare_func_in_func(ids.va_call_f, func);
        let cb = ids.cb.as_ref().map(|cb| {
            // Only what the body's extern calls use.
            let pcs = code_ptr_pcs(body, plan);
            let abi = CAbi::of(module.isa());
            let (mut calls, mut thunks) = (HashMap::new(), HashMap::new());
            for inst in body.blocks.iter().flat_map(|b| &b.insts) {
                let MirInst::ExternCall { sig, args, .. } = inst else { continue };
                let Some((params, ret, va)) = direct_extern(sig, abi) else { continue };
                if let Some(shape) = c_shape(&params, ret, va, sig.params.len())
                    && let Some(&id) = cb.calls.get(&shape)
                {
                    calls.entry(shape).or_insert_with(|| module.declare_func_in_func(id, func));
                }
                for (k, p) in params.iter().enumerate() {
                    if let (CArg::Callback(aux), Some(&pc)) = (p, args.get(3 + k).and_then(|a| pcs.get(a)))
                        && let Some(&id) = cb.thunks.get(&(pc, *aux))
                    {
                        thunks.entry((pc, *aux)).or_insert_with(|| module.declare_func_in_func(id, func));
                    }
                }
            }
            CbRefs { calls, thunks, pcs }
        });
        let defer = module.declare_func_in_func(ids.defer, func);
        let claim = module.declare_func_in_func(ids.claim, func);
        let pad_refs = ids.pad.iter().map(|(&i, &id)| (i, module.declare_func_in_func(id, func))).collect();
        // A view's four words, or a wide lane element's sixteen.
        let view_slot = func.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
            8 * common::PACKED_KIND_WORDS as u32,
            3,
        ));
        let ret = func.signature.returns[0].value_type;
        let ret_hi = func.signature.returns.get(1).map(|r| r.value_type);
        let ret_more: Vec<Type> = func.signature.returns.iter().skip(2).map(|r| r.value_type).collect();
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
            store_index,
            set_field,
            view,
            lane_view,
            str_view,
            map_str_eq,
            fn_entry,
            fn_entry_pair,
            entry_sig,
            c_conv: module.isa().default_call_conv(),
            c_abi: CAbi::of(module.isa()),
            direct_externs: true,
            runtime_ffi: false,
            cow,
            cow_field,
            cow_index,
            rc,
            rc_last,
            rc_reuse,
            rc_take,
            offer_slot,
            alloc,
            array_push,
            array_reserve,
            lane_index,
            lane_store,
            lane_push,
            lanes_to_words,
            lane_index_wide,
            lane_store_wide,
            lane_push_wide,
            probe,
            enum_tag,
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
            ffi_arg,
            ffi_done,
            ffi_str,
            ffi_struct_in,
            ffi_struct_out,
        va_call,
        va_call_f,
            body_index: None,
            pins: false,
            is_pad: false,
            lanes_checked: false,
            lane_bytes: HashMap::new(),
            vec_bytes: HashMap::new(),
            maybe_dropped: HashSet::new(),
            homes_slot: None,
            homes_ptr: None,
            root_handles: Vec::new(),
            keep_slot: None,
            in_range: HashSet::new(),
            calls_slot: None,
            defer,
            claim,
            pad_fn: None,
            pad_refs,
            sites,
            body_id: None,
            cur_loc: None,
            cur_site: None,
            views: HashMap::new(),
            str_views: HashMap::new(),
            payload_views: HashMap::new(),
            product_views: HashMap::new(),
            block_views: HashMap::new(),
            lane_views: HashMap::new(),
            lane_words: HashMap::new(),
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
            ret_more,
            cb,
        }
    }

    /// Whether the direct extern call passing `params` on `args` can run
    /// where a C callback may interrupt it: a trampoline for its shape, a
    /// thunk for each callback, and no buffer lent in place (a collection
    /// may run during it).
    fn cb_call_ok(&self, sig: &compiler::native_mir::MirExternSig, args: &[ValueId], params: &[CArg], ret: CRet, va: Option<CVa>) -> bool {
        let Some(cb) = &self.cb else { return true };
        c_shape(params, ret, va, sig.params.len()).is_some_and(|s| cb.calls.contains_key(&s))
            && params.iter().enumerate().all(|(k, p)| match *p {
                CArg::Ptr(kind) => kind == machine::native::FFI_ARG_CSTR,
                CArg::Callback(aux) => args.get(3 + k).and_then(|a| cb.pcs.get(a)).is_some_and(|&pc| cb.thunks.contains_key(&(pc, aux))),
                _ => true,
            })
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
        if let Some(&(known, _, _)) = self.str_views.get(&v) {
            let zero = self.b.ins().iconst(types::I64, 0);
            self.b.def_var(known, zero);
        }
        if let Some(&(_, len)) = self.payload_views.get(&v).or_else(|| self.product_views.get(&v)) {
            let zero = self.b.ins().iconst(types::I64, 0);
            self.b.def_var(len, zero);
        }
    }

    /// `x` as a `want` word with the same bits. MIR may move a word between
    /// an `f64` and an `i64` value unchanged (a callee's `float` result into
    /// a caller's `int`-typed value, as the runtime's untyped words allow).
    /// A lane shape's three masks as constants, for a lane helper.
    fn lane_shape(&mut self, shape: &compiler::native_mir::MirLaneShape) -> [cranelift_codegen::ir::Value; 3] {
        [shape.tag_mask, shape.payload_mask, shape.unit_mask].map(|m| self.b.ins().iconst(types::I64, m as i64))
    }

    fn coerce(&mut self, x: cranelift_codegen::ir::Value, want: Type) -> cranelift_codegen::ir::Value {
        if self.b.func.dfg.value_type(x) == want {
            x
        } else {
            self.b.ins().bitcast(want, MemFlagsData::new(), x)
        }
    }

    /// Before a call in which a collection (and so a `drop`) or other code
    /// may run: the context's epoch, for [`Self::epoch_check`].
    fn epoch_mark(&mut self) -> Option<Epoch> {
        Some(Epoch(self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_GC_EPOCH)))
    }

    /// After [`Self::epoch_mark`]'s call: when code ran meanwhile (the
    /// epoch moved), re-read every array view.
    fn epoch_check(&mut self, epoch: Option<Epoch>) {
        let Some(Epoch(epoch)) = epoch else {
            return;
        };
        let now = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_GC_EPOCH);
        let moved = self.b.ins().icmp(IntCC::NotEqual, now, epoch);
        let cold = self.b.create_block();
        let go = self.b.create_block();
        self.b.ins().brif(moved, cold, &[], go, &[]);
        self.b.set_cold_block(cold);
        self.b.switch_to_block(cold);
        self.drop_views();
        self.b.ins().jump(go, &[]);
        self.b.switch_to_block(go);
    }

    /// A safepoint: when a collection is due, collect.
    fn safepoint(&mut self) {
        let due = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_GC_DUE);
        let cold = self.b.create_block();
        let go = self.b.create_block();
        self.b.ins().brif(due, cold, &[], go, &[]);
        self.b.set_cold_block(cold);
        self.b.switch_to_block(cold);
        self.b.ins().call(self.collect, &[self.ctx]);
        // Its `drop`s may resize an array.
        self.drop_views();
        self.b.ins().jump(go, &[]);
        self.b.switch_to_block(go);
    }

    /// After an op that may move an array's storage (a push, a host or runtime
    /// call), when other values may name the same array: every view reads
    /// through the helpers until its value is defined again.
    fn drop_views(&mut self) {
        let views: Vec<_> = self.str_views.values().chain(self.lane_views.values()).map(|v| v.0).collect();
        let zero = self.b.ins().iconst(types::I64, 0);
        for ok in views {
            self.b.def_var(ok, zero);
        }
        // A word view is taken again at its next element access
        // ([`Self::ensure_view`]), not read through the helpers from here on.
        let words: Vec<_> = self.views.values().map(|v| v.0).collect();
        if !words.is_empty() {
            let unknown = self.b.ins().iconst(types::I64, VIEW_UNKNOWN);
            for ok in words {
                self.b.def_var(ok, unknown);
            }
        }
    }

    /// Branch to the bail block when `cond` is non-zero, stopping at the
    /// current op for reason `code` (a `TRAP_*`); continue in a new block.
    /// A `VecLoop` reduce with no stores as a native loop over its lanes,
    /// folding left to right as the runtime does. Blocks of [`VEC_BLOCK`]
    /// lanes compute their terms on vectors ([`Self::vlane`]). An int fold
    /// that comes out the same in any order (and / or / xor, a min or max,
    /// an add while no partial can overflow, [`REDUCE_ADD_TERM`]) keeps
    /// one accumulator per lane; any other folds the block's terms in lane
    /// order. A block with a lane that would trap, or an add term out of
    /// that range, and the last lanes, fold one lane at a time. Anything the
    /// runtime's lanes would trap on, or a load outside its array's view,
    /// leaves the loop and runs the whole op on the runtime's lane code from
    /// the start (a pure reduce: nothing it did shows), which gives the
    /// runtime's own result or trap. `false`: not such a reduce (nothing
    /// emitted).
    fn native_reduce(&mut self, f: &MirFunc, inst: &MirInst) -> Result<bool, String> {
        use common::{dense, simd};
        use compiler::native_mir::VecLoopKind;
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
        if !fold_ok || pos.is_some_and(|p| value_used(f, p)) || !self.lanes_native(term, 0) {
            return Ok(false);
        }
        // Stores of numbers into word arrays, as `native_store` takes them.
        for (array, _, ty, value) in stores {
            if !matches!(*ty, dense::TY_I64 | dense::TY_F64) || !self.views.contains_key(array) || !self.lanes_native(value, 0) {
                return Ok(false);
            }
        }
        // Folds whose int result is the same in any order keep a vector
        // accumulator; a min or max needs an order only between ties,
        // which are one value on ints.
        let lanewise = !float
            && match *fold {
                simd::REDUCE_ADD | simd::REDUCE_AND | simd::REDUCE_OR | simd::REDUCE_XOR => true,
                k => (simd::REDUCE_TAKE + dense::CMP_LT..=simd::REDUCE_TAKE + dense::CMP_GE).contains(&k),
            };
        let add = lanewise && *fold == simd::REDUCE_ADD;
        // The fold's own cost per lane, one at a time and on vectors.
        let fold_cost = match *fold {
            simd::REDUCE_ADD if add && Self::lane_is_01(term) => (2.0, 0.5),
            simd::REDUCE_ADD | simd::REDUCE_MUL if !float => (2.0, if add { 1.5 } else { 3.0 }),
            simd::REDUCE_AND | simd::REDUCE_OR | simd::REDUCE_XOR => (1.0, 0.5),
            _ if lanewise => (2.0, 1.5),
            _ => (2.0, 3.0),
        };
        let fold_cost = (fold_cost.0 + stores.len() as f32, fold_cost.1 + stores.len() as f32 * 0.5);
        let vec = Self::vectors_pay(std::iter::once(term).chain(stores.iter().map(|s| &s.3)), fold_cost);
        let lane_ty = if float { types::F64 } else { types::I64 };
        let vty = Self::vec_ty(lane_ty);
        let lo = self.get(f, *from);
        let lo = self.coerce(lo, types::I64);
        let hi = self.get(f, *to);
        let hi = self.coerce(hi, types::I64);
        let acc0 = self.get(f, *init);
        let acc0 = self.coerce(acc0, lane_ty);
        let slow = self.b.create_block();
        let exit = self.b.create_block();
        self.b.append_block_param(exit, lane_ty);
        // lp(p, acc, stop): one lane at a time up to `stop`; after(p, acc):
        // back to the blocks, or out at `hi`.
        let lp = self.b.create_block();
        for t in [types::I64, lane_ty, types::I64] {
            self.b.append_block_param(lp, t);
        }
        let body = self.b.create_block();
        let after = self.b.create_block();
        self.b.append_block_param(after, types::I64);
        self.b.append_block_param(after, lane_ty);
        // venter(p, acc) starts the blocks; vhead(p, acc[, lane
        // accumulators, left]). `nv` vectors per block.
        let venter = self.b.create_block();
        self.b.append_block_param(venter, types::I64);
        self.b.append_block_param(venter, lane_ty);
        let vhead = self.b.create_block();
        self.b.append_block_param(vhead, types::I64);
        self.b.append_block_param(vhead, lane_ty);
        let nv = (VEC_BLOCK / VEC_LANES) as usize;
        if lanewise {
            for _ in 0..nv {
                self.b.append_block_param(vhead, types::I64X2);
            }
        }
        if add {
            self.b.append_block_param(vhead, types::I64);
        }
        // vflush(p, acc, lane accumulators, stop): the lane accumulators
        // into `acc`, then one lane at a time up to `stop`.
        let vflush = self.b.create_block();
        if lanewise {
            self.b.append_block_param(vflush, types::I64);
            self.b.append_block_param(vflush, types::I64);
            for _ in 0..nv {
                self.b.append_block_param(vflush, types::I64X2);
            }
            self.b.append_block_param(vflush, types::I64);
        }
        let vbody = self.b.create_block();
        // A trap in lane `p` with the fold at `acc`: the runtime's lane
        // code from there, lane `p` untouched.
        let resume = self.b.create_block();
        self.b.append_block_param(resume, types::I64);
        self.b.append_block_param(resume, lane_ty);
        let mut refs = Vec::new();
        lane_refs(term, false, &mut refs);
        for (array, off, _, value) in stores {
            lane_refs(value, false, &mut refs);
            refs.push((*array, *off, true));
        }
        self.lanes_guard(f, &refs, lo, hi, slow);
        let limit = self.vec_block_limit(hi);
        self.b.ins().jump(venter, &[lo.into(), acc0.into()]);

        self.b.switch_to_block(venter);
        let (p, acc) = (self.b.block_params(venter)[0], self.b.block_params(venter)[1]);
        if !vec {
            self.b.ins().jump(lp, &[p.into(), acc.into(), hi.into()]);
        } else {
            let mut head_args: Vec<cranelift_codegen::ir::BlockArg> = vec![p.into(), acc.into()];
            if lanewise {
                let ident = match *fold {
                    simd::REDUCE_ADD | simd::REDUCE_OR | simd::REDUCE_XOR => self.vsplat_i(0),
                    simd::REDUCE_AND => self.vsplat_i(-1),
                    _ => self.b.ins().splat(types::I64X2, acc),
                };
                head_args.extend(std::iter::repeat_n(cranelift_codegen::ir::BlockArg::from(ident), nv));
            }
            if add {
                // The lane sums start only from an `acc` that leaves room for
                // `REDUCE_ADD_BLOCKS` blocks of terms below `2^52`.
                let left = self.b.ins().iconst(types::I64, REDUCE_ADD_BLOCKS);
                head_args.push(left.into());
                let room = self.b.ins().iadd_imm_s(acc, 1 << 61);
                let room = self.b.ins().ushr_imm_u(room, 62);
                let go = self.b.create_block();
                self.b.ins().brif(room, lp, &[p.into(), acc.into(), hi.into()], go, &[]);
                self.b.switch_to_block(go);
            }
            self.b.ins().jump(vhead, &head_args);

            self.b.switch_to_block(vhead);
            let hp: Vec<_> = self.b.block_params(vhead).to_vec();
            let (p, acc) = (hp[0], hp[1]);
            let fits = self.vec_block_fits(p, limit);
            // vflush's arguments: from `p` with `acc` and lane accumulators `a`, up to `stop`.
            let flush_args = |p, acc, a: &[cranelift_codegen::ir::Value], stop| {
                let mut v: Vec<cranelift_codegen::ir::BlockArg> = vec![cranelift_codegen::ir::BlockArg::from(p), acc];
                v.extend(a.iter().map(|&x| cranelift_codegen::ir::BlockArg::from(x)));
                v.push(stop);
                v
            };
            if lanewise {
                let args = flush_args(p, acc.into(), &hp[2..2 + nv], hi.into());
                self.b.ins().brif(fits, vbody, &[], vflush, &args);
            } else {
                self.b.ins().brif(fits, vbody, &[], lp, &[p.into(), acc.into(), hi.into()]);
            }

            self.b.switch_to_block(vbody);
            let mut bad = VecBad::default();
            self.vec_bytes.clear();
            let mut terms = Vec::new();
            for half in 0..VEC_BLOCK / VEC_LANES {
                let lp0 = self.b.ins().iadd_imm_s(p, half * VEC_LANES);
                let at = VecAt { p0: p, half: half as usize, p: lp0 };
                let t = self.vlane(f, term, at, &mut bad);
                terms.push(self.vcoerce(t, vty));
            }
            // The stores' values too; they are stored once nothing of the
            // block traps, so a block run again lane by lane reads what the
            // scalar loop does.
            let mut values = Vec::with_capacity(stores.len() * nv);
            for (_, _, ty, value) in stores {
                let want = if *ty == dense::TY_F64 { types::F64X2 } else { types::I64X2 };
                for half in 0..VEC_BLOCK / VEC_LANES {
                    let lp0 = self.b.ins().iadd_imm_s(p, half * VEC_LANES);
                    let at = VecAt { p0: p, half: half as usize, p: lp0 };
                    let x = self.vlane(f, value, at, &mut bad);
                    values.push(self.vcoerce(x, want));
                }
            }
            let stop = self.b.ins().iadd_imm_s(p, VEC_BLOCK);
            if lanewise {
                if add {
                    // A term outside `-2^52 .. 2^52` folds lane by lane.
                    // A 0 / 1 term is in range. Biased, a term in range is
                    // below `2^53`, so the bits above of all of them at once.
                    if !Self::lane_is_01(term) {
                        let bias = self.vsplat_i(1 << REDUCE_ADD_TERM);
                        let mut any = None;
                        for &t in &terms {
                            let u = self.b.ins().iadd(t, bias);
                            any = Some(match any {
                                None => u,
                                Some(a) => self.b.ins().bor(a, u),
                            });
                        }
                        let h = self.b.ins().ushr_imm_u(any.expect("a term per vector"), REDUCE_ADD_TERM + 1);
                        bad.nonzero(self, h);
                    }
                }
                if let Some(trap) = bad.any(self) {
                    let go = self.b.create_block();
                    let args = flush_args(p, acc.into(), &hp[2..2 + nv], stop.into());
                    self.b.ins().brif(trap, vflush, &args, go, &[]);
                    self.b.switch_to_block(go);
                }
                self.vec_stores(f, stores, p, &values);
                let mut lanes = Vec::new();
                for (k, &t) in terms.iter().enumerate() {
                    let a = hp[2 + k];
                    lanes.push(match *fold {
                        simd::REDUCE_ADD => self.b.ins().iadd(a, t),
                        simd::REDUCE_AND => self.b.ins().band(a, t),
                        simd::REDUCE_OR => self.b.ins().bor(a, t),
                        simd::REDUCE_XOR => self.b.ins().bxor(a, t),
                        k => {
                            let take = self.vlane_cmp(k - simd::REDUCE_TAKE, false, t, a);
                            self.b.ins().bitselect(take, t, a)
                        }
                    });
                }
                let mut next: Vec<cranelift_codegen::ir::BlockArg> = vec![stop.into(), acc.into()];
                next.extend(lanes.iter().map(|&x| cranelift_codegen::ir::BlockArg::from(x)));
                if add {
                    // Out of blocks: the lane sums into `acc`, which starts again.
                    let left = self.b.ins().iadd_imm_s(hp[2 + nv], -1);
                    next.push(left.into());
                    let go = self.b.create_block();
                    let args = flush_args(stop, acc.into(), &lanes, stop.into());
                    self.b.ins().brif(left, go, &[], vflush, &args);
                    self.b.switch_to_block(go);
                }
                self.b.ins().jump(vhead, &next);
                // vflush
                self.b.switch_to_block(vflush);
                let fp: Vec<_> = self.b.block_params(vflush).to_vec();
                let mut acc = fp[1];
                for &v in &fp[2..2 + nv] {
                    for k in 0..VEC_LANES as u8 {
                        let x = self.b.ins().extractlane(v, k);
                        acc = match *fold {
                            // In range: no partial overflows (see `REDUCE_ADD_BLOCKS`).
                            simd::REDUCE_ADD => self.b.ins().iadd(acc, x),
                            simd::REDUCE_AND => self.b.ins().band(acc, x),
                            simd::REDUCE_OR => self.b.ins().bor(acc, x),
                            simd::REDUCE_XOR => self.b.ins().bxor(acc, x),
                            k => {
                                let take = self.lane_cmp(k - simd::REDUCE_TAKE, false, x, acc);
                                self.b.ins().select(take, x, acc)
                            }
                        };
                    }
                }
                self.b.ins().jump(lp, &[fp[0].into(), acc.into(), fp[2 + nv].into()]);
            } else {
                // In lane order, each step as the scalar loop takes it.
                let mut next = acc;
                for &t in &terms {
                    for k in 0..VEC_LANES as u8 {
                        let x = self.b.ins().extractlane(t, k);
                        let (r, of) = self.fold_step(*fold, float, next, x);
                        if let Some(of) = of {
                            bad.scalar(self, of);
                        }
                        next = r;
                    }
                }
                if let Some(trap) = bad.any(self) {
                    let go = self.b.create_block();
                    self.b.ins().brif(trap, lp, &[p.into(), acc.into(), stop.into()], go, &[]);
                    self.b.switch_to_block(go);
                }
                self.vec_stores(f, stores, p, &values);
                self.b.ins().jump(vhead, &[stop.into(), next.into()]);
            }
        }

        self.b.switch_to_block(lp);
        let (p, acc, stop) = (self.b.block_params(lp)[0], self.b.block_params(lp)[1], self.b.block_params(lp)[2]);
        let done = self.b.ins().icmp(IntCC::SignedGreaterThanOrEqual, p, stop);
        self.b.ins().brif(done, after, &[p.into(), acc.into()], body, &[]);
        self.b.switch_to_block(body);
        // Every value and the fold first: a trap leaves lane `p` untouched.
        let lslow = self.b.create_block();
        self.lanes_checked = true;
        let mut values = Vec::with_capacity(stores.len());
        for (_, _, _, value) in stores {
            let v = self.lane(f, value, p, lslow);
            values.push(self.coerce(v, types::I64));
        }
        let x = self.lane(f, term, p, lslow);
        self.lanes_checked = false;
        let x = self.coerce(x, lane_ty);
        let (next, of) = self.fold_step(*fold, float, acc, x);
        if let Some(of) = of {
            self.leave_if(of, lslow);
        }
        for ((array, off, _, _), w) in stores.iter().zip(values) {
            let o = self.get(f, *off);
            let o = self.coerce(o, types::I64);
            let i = self.b.ins().iadd(p, o);
            let at = self.element(*array, i);
            self.b.ins().store(MemFlagsData::trusted(), w, at, 0);
        }
        let p1 = self.b.ins().iadd_imm_s(p, 1);
        self.b.ins().jump(lp, &[p1.into(), next.into(), stop.into()]);
        self.b.switch_to_block(lslow);
        self.b.ins().jump(resume, &[p.into(), acc.into()]);
        self.b.switch_to_block(resume);
        let (at, acc_at) = (self.b.block_params(resume)[0], self.b.block_params(resume)[1]);
        let acc_w = self.coerce(acc_at, types::I64);
        let (_, frame) = self.vector_lanes_from(f, inst, Some(at), Some(acc_w));
        let w = self.b.ins().stack_load(types::I64, lane_ty, frame, 0);
        if !stores.is_empty() {
            self.drop_views();
        }
        self.b.ins().jump(exit, &[w.into()]);
        self.b.switch_to_block(after);
        let (p, acc) = (self.b.block_params(after)[0], self.b.block_params(after)[1]);
        let end = self.b.ins().icmp(IntCC::SignedGreaterThanOrEqual, p, hi);
        self.b.ins().brif(end, exit, &[acc.into()], venter, &[p.into(), acc.into()]);
        self.b.switch_to_block(slow);
        // An access the guard could not show in place: the whole op on the
        // runtime. The loop's result is its `dest` slot, as `VLoop` leaves it.
        self.b.ins().jump(resume, &[lo.into(), acc0.into()]);
        self.b.switch_to_block(exit);
        let r = self.b.block_params(exit)[0];
        self.set(f, *dest, r);
        if let Some(p) = pos {
            let zero = self.b.ins().iconst(types::I64, 0);
            self.set(f, *p, zero);
        }
        Ok(true)
    }

    /// A block's store values (four vectors per store, in `stores`' order)
    /// into their word arrays at lanes `p ..`.
    fn vec_stores(&mut self, f: &MirFunc, stores: &[(ValueId, ValueId, u8, compiler::native_mir::VecExpr)], p: cranelift_codegen::ir::Value, values: &[cranelift_codegen::ir::Value]) {
        let nv = (VEC_BLOCK / VEC_LANES) as usize;
        for ((array, off, _, _), w) in stores.iter().zip(values.chunks(nv)) {
            let o = self.get(f, *off);
            let o = self.coerce(o, types::I64);
            let i = self.b.ins().iadd(p, o);
            let at = self.element(*array, i);
            for (k, &v) in w.iter().enumerate() {
                self.b.ins().store(MemFlagsData::new().with_notrap(), v, at, (k as i64 * VEC_LANES * 8) as i32);
            }
        }
    }

    /// `acc ⊕ x` for fold `fold` (a `REDUCE_*`), and for an int add or
    /// multiply its overflow flag (an `i8`; the runtime traps there).
    fn fold_step(
        &mut self,
        fold: u8,
        float: bool,
        acc: cranelift_codegen::ir::Value,
        x: cranelift_codegen::ir::Value,
    ) -> (cranelift_codegen::ir::Value, Option<cranelift_codegen::ir::Value>) {
        use common::simd;
        match fold {
            simd::REDUCE_ADD if float => (self.b.ins().fadd(acc, x), None),
            simd::REDUCE_MUL if float => (self.b.ins().fmul(acc, x), None),
            simd::REDUCE_ADD => {
                let (r, of) = self.b.ins().sadd_overflow(acc, x);
                (r, Some(of))
            }
            simd::REDUCE_MUL => {
                let (r, of) = self.b.ins().smul_overflow(acc, x);
                (r, Some(of))
            }
            simd::REDUCE_AND => (self.b.ins().band(acc, x), None),
            simd::REDUCE_OR => (self.b.ins().bor(acc, x), None),
            simd::REDUCE_XOR => (self.b.ins().bxor(acc, x), None),
            k => {
                let take = self.lane_cmp(k - simd::REDUCE_TAKE, float, x, acc);
                (self.b.ins().select(take, x, acc), None)
            }
        }
    }

    /// A `VecLoop` of stores as a native loop. Blocks of [`VEC_BLOCK`] lanes
    /// run on vectors ([`Self::vlane`]): every value of the block is read
    /// and computed first, then all are stored. `vec_loop` takes only bodies
    /// whose reads come before their stores and no read below a store's
    /// offset, so a block in that order stores what the scalar loop does.
    /// A block with a lane that would trap, and the last lanes, run one
    /// lane at a time: every value of lane `p` is computed, then stored. A
    /// trap there, or an element outside its view, leaves with lane `p`
    /// untouched and runs the rest, from `p`, on the runtime's lane code.
    /// `false`: not such a loop (nothing emitted).
    fn native_store(&mut self, f: &MirFunc, inst: &MirInst) -> Result<bool, String> {
        use common::dense;
        use compiler::native_mir::VecLoopKind;
        let MirInst::VecLoop { dest, pos, from, to, kind: VecLoopKind::Store { stores } } = inst else {
            return Ok(false);
        };
        // Lanes are numbers: a store of one over another changes no count.
        // Any other element is left to the runtime, which counts it.
        if !stores.iter().all(|s| matches!(s.2, dense::TY_I64 | dense::TY_F64)) {
            return Ok(false);
        }
        if stores.is_empty() || pos.is_some_and(|p| value_used(f, p)) {
            return Ok(false);
        }
        let words_ok = stores.iter().all(|(array, _, ty, value)| matches!(*ty, dense::TY_I64 | dense::TY_F64) && self.views.contains_key(array) && self.lanes_native(value, 0));
        // Bytes: each store goes into packed bytes, every source can be read
        // as bytes, every lane is an int.
        let mut reads = Vec::new();
        for (_, _, _, value) in stores {
            lane_refs(value, false, &mut reads);
        }
        let bytes_ok = stores.iter().all(|s| s.2 == dense::TY_I64 && self.lanes_native_bytes(&s.3, 0)) && reads.iter().all(|&(a, _, _)| self.str_views.contains_key(&a));
        if !words_ok && !bytes_ok {
            return Ok(false);
        }
        // Each store's value, and the store, per lane.
        let vec = Self::vectors_pay(stores.iter().map(|s| &s.3), (stores.len() as f32, stores.len() as f32 * 0.5));
        let whole = self.b.create_block();
        let exit = self.b.create_block();
        let try_bytes = self.b.create_block();
        let rt = self.b.create_block();
        self.b.append_block_param(rt, types::I64);
        let lo = self.get(f, *from);
        let lo = self.coerce(lo, types::I64);
        let hi = self.get(f, *to);
        let hi = self.coerce(hi, types::I64);
        if words_ok {
            let mut refs = Vec::new();
            for (array, off, _, value) in stores {
                lane_refs(value, false, &mut refs);
                refs.push((*array, *off, true));
            }
            self.lanes_guard(f, &refs, lo, hi, try_bytes);
            self.store_loop(f, stores, false, vec, lo, hi, rt, exit);
        } else {
            self.b.ins().jump(try_bytes, &[]);
        }
        self.b.switch_to_block(try_bytes);
        if bytes_ok {
            self.bytes_guard(f, &reads, lo, hi, whole);
            self.bytes_store_guard(f, stores, lo, hi, whole);
            self.store_loop(f, stores, true, vec, lo, hi, rt, exit);
            self.lane_bytes.clear();
        } else {
            self.b.ins().jump(whole, &[]);
        }
        // An access the guard could not show in place: the whole op there.
        self.b.switch_to_block(whole);
        self.b.ins().jump(rt, &[lo.into()]);
        // The runtime's lane code from lane `at` on: one call for every
        // way out of the native loop, as a copy of the op is long.
        self.b.switch_to_block(rt);
        let at = self.b.block_params(rt)[0];
        self.vector_lanes_at(f, inst, Some(at));
        self.drop_views();
        self.b.ins().jump(exit, &[]);
        self.b.switch_to_block(exit);
        let zero = self.b.ins().iconst(types::I64, 0);
        self.set(f, *dest, zero);
        if let Some(p) = pos {
            self.set(f, *p, zero);
        }
        Ok(true)
    }

    /// In front of [`Self::native_store`]'s loop over bytes, after
    /// [`Self::bytes_guard`]: each store's array holds packed bytes, and
    /// every lane `lo .. hi` stores in them, or branch to `fail`. Their
    /// bytes go into [`Self::lane_bytes`].
    fn bytes_store_guard(&mut self, f: &MirFunc, stores: &[(ValueId, ValueId, u8, compiler::native_mir::VecExpr)], lo: cranelift_codegen::ir::Value, hi: cranelift_codegen::ir::Value, fail: Block) {
        let l = machine::native::array_slots();
        let t = MemFlagsData::trusted();
        for (array, off, _, _) in stores {
            let a = self.get(f, *array);
            let a = self.coerce(a, types::I64);
            let (kind_b, tag_b, go) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
            self.b.ins().brif(a, kind_b, &[], fail, &[]);
            self.b.switch_to_block(kind_b);
            let k = self.b.ins().uload8(types::I64, t, a, l.kind_at as i32);
            let is = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(l.kind));
            self.b.ins().brif(is, tag_b, &[], fail, &[]);
            self.b.switch_to_block(tag_b);
            let tag = self.b.ins().uload8(types::I64, t, a, l.data_at as i32);
            let is = self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(l.bytes_tag));
            let ptr = self.b.ins().load(types::I64, t, a, l.bytes_ptr_at as i32);
            let len = self.b.ins().load(types::I64, t, a, l.bytes_len_at as i32);
            let o = self.get(f, *off);
            let o = self.coerce(o, types::I64);
            let (first, of1) = self.b.ins().sadd_overflow(lo, o);
            let (last, of2) = self.b.ins().sadd_overflow(hi, o);
            let of = self.b.ins().bor(of1, of2);
            let fine = self.b.ins().icmp_imm_s(IntCC::Equal, of, 0);
            let ge = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, first, 0);
            let le = self.b.ins().icmp(IntCC::SignedLessThanOrEqual, last, len);
            let x = self.b.ins().band(is, fine);
            let y = self.b.ins().band(ge, le);
            let all = self.b.ins().band(x, y);
            self.b.ins().brif(all, go, &[], fail, &[]);
            self.b.switch_to_block(go);
            self.lane_bytes.insert(*array, ptr);
        }
    }

    /// [`Self::native_store`]'s loop for one layout, after its guard: word
    /// arrays through their views, or (`bytes`) packed bytes through
    /// [`Self::lane_bytes`]. Blocks on vectors when `vec`, else one lane at
    /// a time, to `exit`. A trap, and over bytes a value that is no byte,
    /// goes to `rt`, the runtime's lane code from its lane on.
    #[allow(clippy::too_many_arguments)]
    fn store_loop(
        &mut self,
        f: &MirFunc,
        stores: &[(ValueId, ValueId, u8, compiler::native_mir::VecExpr)],
        bytes: bool,
        vec: bool,
        lo: cranelift_codegen::ir::Value,
        hi: cranelift_codegen::ir::Value,
        rt: Block,
        exit: Block,
    ) {
        use common::dense;
        // vhead(p): a whole block left? vbody: the block on vectors.
        // lp(p, stop): one lane at a time up to `stop`; after(p): on to
        // the next block, or out at `hi`.
        let vhead = self.b.create_block();
        self.b.append_block_param(vhead, types::I64);
        let vbody = self.b.create_block();
        let lp = self.b.create_block();
        self.b.append_block_param(lp, types::I64);
        self.b.append_block_param(lp, types::I64);
        let body = self.b.create_block();
        let after = self.b.create_block();
        self.b.append_block_param(after, types::I64);
        let slow = self.b.create_block();
        let limit = self.vec_block_limit(hi);
        self.b.ins().jump(vhead, &[lo.into()]);

        self.b.switch_to_block(vhead);
        let p = self.b.block_params(vhead)[0];
        if !vec {
            self.b.ins().jump(lp, &[p.into(), hi.into()]);
        } else {
            let fits = self.vec_block_fits(p, limit);
            self.b.ins().brif(fits, vbody, &[], lp, &[p.into(), hi.into()]);
            self.b.switch_to_block(vbody);
            let mut bad = VecBad::default();
            self.vec_bytes.clear();
            let mut values = Vec::with_capacity(stores.len() * 4);
            for (_, _, ty, value) in stores {
                let want = if *ty == dense::TY_F64 { types::F64X2 } else { types::I64X2 };
                for half in 0..VEC_BLOCK / VEC_LANES {
                    let lp0 = self.b.ins().iadd_imm_s(p, half * VEC_LANES);
                    let at = VecAt { p0: p, half: half as usize, p: lp0 };
                    let x = self.vlane(f, value, at, &mut bad);
                    values.push(self.vcoerce(x, want));
                }
            }
            if bytes {
                // A value that is no byte stores one lane at a time.
                let mut any = values[0];
                for &v in &values[1..] {
                    any = self.b.ins().bor(any, v);
                }
                let h = self.b.ins().ushr_imm_u(any, 8);
                bad.nonzero(self, h);
            }
            let stop = self.b.ins().iadd_imm_s(p, VEC_BLOCK);
            if let Some(trap) = bad.any(self) {
                let go = self.b.create_block();
                self.b.ins().brif(trap, lp, &[p.into(), stop.into()], go, &[]);
                self.b.switch_to_block(go);
            }
            let quarters = VEC_BLOCK as usize / VEC_LANES as usize;
            for ((array, off, _, _), w) in stores.iter().zip(values.chunks(quarters)) {
                let o = self.get(f, *off);
                let o = self.coerce(o, types::I64);
                let i = self.b.ins().iadd(p, o);
                if bytes {
                    let w = self.vec_pack_bytes(w);
                    let at = self.b.ins().iadd(self.lane_bytes[array], i);
                    self.b.ins().store(MemFlagsData::new().with_notrap(), w, at, 0);
                } else {
                    let at = self.element(*array, i);
                    for (k, &v) in w.iter().enumerate() {
                        self.b.ins().store(MemFlagsData::new().with_notrap(), v, at, (k as i64 * VEC_LANES * 8) as i32);
                    }
                }
            }
            self.b.ins().jump(vhead, &[stop.into()]);
        }

        self.b.switch_to_block(lp);
        let p = self.b.block_params(lp)[0];
        let stop = self.b.block_params(lp)[1];
        let done = self.b.ins().icmp(IntCC::SignedGreaterThanOrEqual, p, stop);
        self.b.ins().brif(done, after, &[p.into()], body, &[]);
        self.b.switch_to_block(body);
        self.lanes_checked = true;
        let mut values = Vec::with_capacity(stores.len());
        for (_, _, _, value) in stores {
            let x = self.lane(f, value, p, slow);
            values.push(self.coerce(x, types::I64));
        }
        self.lanes_checked = false;
        if bytes {
            let mut any = values[0];
            for &v in &values[1..] {
                any = self.b.ins().bor(any, v);
            }
            let no_byte = self.b.ins().icmp_imm_u(IntCC::UnsignedGreaterThan, any, 255);
            let go = self.b.create_block();
            self.b.ins().brif(no_byte, slow, &[], go, &[]);
            self.b.switch_to_block(go);
        }
        for ((array, off, _, _), w) in stores.iter().zip(values) {
            let o = self.get(f, *off);
            let o = self.coerce(o, types::I64);
            let i = self.b.ins().iadd(p, o);
            if bytes {
                let at = self.b.ins().iadd(self.lane_bytes[array], i);
                self.b.ins().istore8(MemFlagsData::trusted(), w, at, 0);
            } else {
                let at = self.element(*array, i);
                self.b.ins().store(MemFlagsData::trusted(), w, at, 0);
            }
        }
        let p1 = self.b.ins().iadd_imm_s(p, 1);
        self.b.ins().jump(lp, &[p1.into(), stop.into()]);
        self.b.switch_to_block(after);
        let q = self.b.block_params(after)[0];
        let end = self.b.ins().icmp(IntCC::SignedGreaterThanOrEqual, q, hi);
        self.b.ins().brif(end, exit, &[], vhead, &[q.into()]);
        // A trap in lane `p`: the runtime's lane code from `p` on.
        self.b.switch_to_block(slow);
        self.b.ins().jump(rt, &[p.into()]);
    }

    /// In front of a native vector loop up to `hi`: the bound below which
    /// a block may start, so that its lanes `p .. p + VEC_BLOCK` are all
    /// below `hi` ([`Self::vec_block_fits`]). Near `i64::MIN` no block fits.
    fn vec_block_limit(&mut self, hi: cranelift_codegen::ir::Value) -> cranelift_codegen::ir::Value {
        let room = self.b.ins().iconst(types::I64, VEC_BLOCK - 1);
        let (limit, under) = self.b.ins().ssub_overflow(hi, room);
        let min = self.b.ins().iconst(types::I64, i64::MIN);
        self.b.ins().select(under, min, limit)
    }

    /// Whether a block starting at `p` fits below the loop's bound (an
    /// `i8`), given [`Self::vec_block_limit`]'s `limit`.
    fn vec_block_fits(&mut self, p: cranelift_codegen::ir::Value, limit: cranelift_codegen::ir::Value) -> cranelift_codegen::ir::Value {
        self.b.ins().icmp(IntCC::SignedLessThan, p, limit)
    }

    /// A `VecLoop` push (`out.push(value(p))` for each lane, in order) as a
    /// native loop. Two layouts run in place: `out` and every array the
    /// body reads hold words (`int` / `float` lanes), or `out` holds packed
    /// bytes and every source is a string or packed bytes (`int` lanes
    /// that are bytes: [`Self::lane_bytes`]). A block of [`VEC_BLOCK`] lanes
    /// computes its values on vectors, then appends them in one store when
    /// `out` has room for them (and, for bytes, each value is one). A block
    /// with a lane that would trap, a value that is no byte, a full `out`,
    /// and the last lanes push one lane at a time through the runtime's
    /// push, which grows `out`. A trap leaves the pushes before it and runs
    /// the rest, from that lane, on the runtime's lane code; any other
    /// layout runs the whole op there. `false`: not such a loop.
    fn native_push(&mut self, f: &MirFunc, inst: &MirInst) -> Result<bool, String> {
        use common::dense;
        use compiler::native_mir::VecLoopKind;
        let MirInst::VecLoop { dest, pos, from, to, kind: VecLoopKind::Push { out, ty, value } } = inst else {
            return Ok(false);
        };
        if !matches!(*ty, dense::TY_I64 | dense::TY_F64) || pos.is_some_and(|p| value_used(f, p)) {
            return Ok(false);
        }
        // The same value each lane: MIR fills that in one host call
        // (`vec_fill`), and the loop it keeps runs only where the fill did
        // not apply. The runtime takes it there; native code for it would
        // only grow the function.
        if !lane_varies(value) {
            return Ok(false);
        }
        let mut reads = Vec::new();
        lane_refs(value, false, &mut reads);
        let words_ok = self.lanes_native(value, 0);
        // Bytes: every source can be read as bytes, every lane is an int.
        let bytes_ok = *ty == dense::TY_I64 && reads.iter().all(|&(a, _, _)| self.str_views.contains_key(&a)) && self.lanes_native_bytes(value, 0);
        if !words_ok && !bytes_ok {
            return Ok(false);
        }
        let l = machine::native::array_slots();
        let t = MemFlagsData::trusted();
        let lo = self.get(f, *from);
        let lo = self.coerce(lo, types::I64);
        let hi = self.get(f, *to);
        let hi = self.coerce(hi, types::I64);
        let a = self.get(f, *out);
        let a = self.coerce(a, types::I64);
        let whole = self.b.create_block();
        let exit = self.b.create_block();
        let rt = self.b.create_block();
        self.b.append_block_param(rt, types::I64);
        // `out`'s layout: words, packed bytes, or anything else (the runtime).
        let (kind_b, tag_b, words_b, bytes_b) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.ins().brif(a, kind_b, &[], whole, &[]);
        self.b.switch_to_block(kind_b);
        let k = self.b.ins().uload8(types::I64, t, a, l.kind_at as i32);
        let is = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(l.kind));
        self.b.ins().brif(is, tag_b, &[], whole, &[]);
        self.b.switch_to_block(tag_b);
        let tag = self.b.ins().uload8(types::I64, t, a, l.data_at as i32);
        let is_words = self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(l.words_tag));
        let is_bytes = self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(l.bytes_tag));
        let other = self.b.create_block();
        self.b.ins().brif(is_words, words_b, &[], other, &[]);
        self.b.switch_to_block(other);
        self.b.ins().brif(is_bytes, bytes_b, &[], whole, &[]);
        let vec = l.inline_push && Self::vectors_pay([value], (4.0, 1.0));
        for bytes in [false, true] {
            self.b.switch_to_block(if bytes { bytes_b } else { words_b });
            if (bytes && !bytes_ok) || (!bytes && !words_ok) {
                self.b.ins().jump(whole, &[]);
                continue;
            }
            if bytes {
                self.bytes_guard(f, &reads, lo, hi, whole);
            } else {
                self.lanes_guard(f, &reads, lo, hi, whole);
            }
            self.push_loop(f, value, a, bytes, vec, lo, hi, rt, exit);
            self.lane_bytes.clear();
        }
        // An access the guard could not show in place: the whole op there.
        self.b.switch_to_block(whole);
        self.b.ins().jump(rt, &[lo.into()]);
        // The runtime's lane code from lane `at` on: one call for every
        // way out of the native loop, as a copy of the op is long.
        self.b.switch_to_block(rt);
        let at = self.b.block_params(rt)[0];
        self.vector_lanes_at(f, inst, Some(at));
        self.b.ins().jump(exit, &[]);
        self.b.switch_to_block(exit);
        // A push keeps the array's identity; its storage may have moved.
        self.drop_views();
        self.set(f, *dest, a);
        if let Some(p) = pos {
            let zero = self.b.ins().iconst(types::I64, 0);
            self.set(f, *p, zero);
        }
        Ok(true)
    }

    /// [`Self::native_push`]'s loop for one layout of `out` (`a`), after its
    /// guard, to `exit` (or `rt`, the runtime's lane code from a lane on).
    /// With `vec`, blocks of [`VEC_BLOCK`] lanes append on
    /// vectors into room `out` already has, its length kept in a register
    /// and stored when the run ends; with too little room, `out` grows
    /// first ([`machine::native::coil_native_array_reserve`]: to twice its
    /// length at most, so a loop that traps early does not take room for
    /// all its lanes). A block that would trap (for bytes, or hold a value
    /// that is no byte) and the last lanes push one lane at a time through
    /// the runtime's push.
    #[allow(clippy::too_many_arguments)]
    fn push_loop(
        &mut self,
        f: &MirFunc,
        value: &compiler::native_mir::VecExpr,
        a: cranelift_codegen::ir::Value,
        bytes: bool,
        vec: bool,
        lo: cranelift_codegen::ir::Value,
        hi: cranelift_codegen::ir::Value,
        rt: Block,
        exit: Block,
    ) {
        let l = machine::native::array_slots();
        let t = MemFlagsData::trusted();
        let float = !bytes && Self::lane_is_float(value);
        let vty = if float { types::F64X2 } else { types::I64X2 };
        let (len_at, cap_at, ptr_at) = if bytes { (l.bytes_len_at, l.bytes_cap_at, l.bytes_ptr_at) } else { (l.len_at, l.cap_at, l.ptr_at) };
        let lp = self.b.create_block();
        self.b.append_block_param(lp, types::I64);
        self.b.append_block_param(lp, types::I64);
        let body = self.b.create_block();
        let after = self.b.create_block();
        self.b.append_block_param(after, types::I64);
        let slow = self.b.create_block();
        // enter(p): a run of blocks from lane `p`, or the last lanes.
        let enter = self.b.create_block();
        self.b.append_block_param(enter, types::I64);
        if !vec {
            self.b.ins().jump(lp, &[lo.into(), hi.into()]);
        } else {
            self.b.ins().jump(enter, &[lo.into()]);
            self.b.switch_to_block(enter);
            let p = self.b.block_params(enter)[0];
            let (blocks, room_b, grow, run) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
            for _ in 0..2 {
                self.b.append_block_param(run, types::I64);
            }
            let (rbody, rbad, rdone) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
            for blk in [rbad, rdone] {
                self.b.append_block_param(blk, types::I64);
                self.b.append_block_param(blk, types::I64);
            }
            // A whole block left, and `out` still in this layout (a value
            // that is no byte, pushed one lane at a time, turns bytes into
            // words).
            let (rest, of) = self.b.ins().ssub_overflow(hi, p);
            let one = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, rest, VEC_BLOCK);
            let fine = self.b.ins().icmp_imm_s(IntCC::Equal, of, 0);
            let tag = self.b.ins().uload8(types::I64, t, a, l.data_at as i32);
            let want = if bytes { l.bytes_tag } else { l.words_tag };
            let same = self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(want));
            let go = self.b.ins().band(one, fine);
            let go = self.b.ins().band(go, same);
            self.b.ins().brif(go, blocks, &[], lp, &[p.into(), hi.into()]);
            self.b.switch_to_block(blocks);
            let len = self.b.ins().load(types::I64, t, a, len_at as i32);
            let cap = self.b.ins().load(types::I64, t, a, cap_at as i32);
            let room = self.b.ins().isub(cap, len);
            let has = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, room, VEC_BLOCK);
            self.b.ins().brif(has, room_b, &[], grow, &[]);
            // Room for `min(rest, max(len, 64))` more, then again from `p`.
            self.b.switch_to_block(grow);
            let floor = self.b.ins().iconst(types::I64, 64);
            let want_n = self.b.ins().smax(len, floor);
            let want_n = self.b.ins().smin(want_n, rest);
            self.call_checked(self.array_reserve, &[self.ctx, a, want_n]);
            self.b.ins().jump(enter, &[p.into()]);
            // A run while there is room: blocks start below `end - 7`.
            self.b.switch_to_block(room_b);
            let ptr = self.b.ins().load(types::I64, t, a, ptr_at as i32);
            let avail = self.b.ins().smin(room, rest);
            let end = self.b.ins().iadd(p, avail);
            let limit = self.b.ins().iadd_imm_s(end, -(VEC_BLOCK - 1));
            if !bytes {
                let one = self.b.ins().iconst(types::I8, 1);
                self.b.ins().store(t, one, a, l.refs_at as i32);
            }
            self.b.ins().jump(run, &[p.into(), len.into()]);
            self.b.switch_to_block(run);
            let (p, n) = (self.b.block_params(run)[0], self.b.block_params(run)[1]);
            let fits = self.vec_block_fits(p, limit);
            self.b.ins().brif(fits, rbody, &[], rdone, &[p.into(), n.into()]);
            self.b.switch_to_block(rbody);
            let (values, bad) = self.push_values(f, value, p, bytes, vty);
            if let Some(b) = bad.any(self) {
                let go = self.b.create_block();
                self.b.ins().brif(b, rbad, &[p.into(), n.into()], go, &[]);
                self.b.switch_to_block(go);
            }
            self.push_store(ptr, n, &values, bytes);
            let stop = self.b.ins().iadd_imm_s(p, VEC_BLOCK);
            let n2 = self.b.ins().iadd_imm_s(n, VEC_BLOCK);
            self.b.ins().jump(run, &[stop.into(), n2.into()]);
            // A block that would trap, or a value that is no byte: one lane
            // at a time, then on from the next block.
            self.b.switch_to_block(rbad);
            let (p, n) = (self.b.block_params(rbad)[0], self.b.block_params(rbad)[1]);
            self.b.ins().store(t, n, a, len_at as i32);
            let stop = self.b.ins().iadd_imm_s(p, VEC_BLOCK);
            self.b.ins().jump(lp, &[p.into(), stop.into()]);
            // Out of room or of whole blocks: on from `p`.
            self.b.switch_to_block(rdone);
            let (p, n) = (self.b.block_params(rdone)[0], self.b.block_params(rdone)[1]);
            self.b.ins().store(t, n, a, len_at as i32);
            self.b.ins().jump(enter, &[p.into()]);
        }
        // One lane at a time, through the runtime's push.
        self.b.switch_to_block(lp);
        let (p, stop) = (self.b.block_params(lp)[0], self.b.block_params(lp)[1]);
        let done = self.b.ins().icmp(IntCC::SignedGreaterThanOrEqual, p, stop);
        self.b.ins().brif(done, after, &[p.into()], body, &[]);
        self.b.switch_to_block(body);
        self.lanes_checked = true;
        let x = self.lane(f, value, p, slow);
        self.lanes_checked = false;
        let x = self.coerce(x, types::I64);
        self.call_checked(self.array_push, &[self.ctx, a, x]);
        let p1 = self.b.ins().iadd_imm_s(p, 1);
        self.b.ins().jump(lp, &[p1.into(), stop.into()]);
        self.b.switch_to_block(after);
        let q = self.b.block_params(after)[0];
        let end = self.b.ins().icmp(IntCC::SignedGreaterThanOrEqual, q, hi);
        if vec {
            self.b.ins().brif(end, exit, &[], enter, &[q.into()]);
        } else {
            self.b.ins().jump(exit, &[]);
            let _ = end;
        }
        // A trap in lane `p`: the runtime's lane code from `p` on.
        self.b.switch_to_block(slow);
        self.b.ins().jump(rt, &[p.into()]);
    }

    /// [`Self::push_loop`]'s values of the block from lane `p`, four
    /// vectors of `vty`, and what would trap (for bytes, also a value that
    /// is no byte: it goes through the runtime's push).
    fn push_values(&mut self, f: &MirFunc, value: &compiler::native_mir::VecExpr, p: cranelift_codegen::ir::Value, bytes: bool, vty: Type) -> (Vec<cranelift_codegen::ir::Value>, VecBad) {
        let mut bad = VecBad::default();
        self.vec_bytes.clear();
        let mut values = Vec::new();
        for half in 0..VEC_BLOCK / VEC_LANES {
            let lp0 = self.b.ins().iadd_imm_s(p, half * VEC_LANES);
            let at = VecAt { p0: p, half: half as usize, p: lp0 };
            let x = self.vlane(f, value, at, &mut bad);
            values.push(self.vcoerce(x, vty));
        }
        if bytes {
            let mut any = values[0];
            for &v in &values[1..] {
                any = self.b.ins().bor(any, v);
            }
            let h = self.b.ins().ushr_imm_u(any, 8);
            bad.nonzero(self, h);
        }
        (values, bad)
    }

    /// Append a block's values at element `len` of `out`'s storage `ptr`,
    /// which has room for them; the caller sets the length (and, for
    /// words, marks the array as a store does).
    fn push_store(&mut self, ptr: cranelift_codegen::ir::Value, len: cranelift_codegen::ir::Value, values: &[cranelift_codegen::ir::Value], bytes: bool) {
        if bytes {
            let w = self.vec_pack_bytes(values);
            let at = self.b.ins().iadd(ptr, len);
            self.b.ins().store(MemFlagsData::new().with_notrap(), w, at, 0);
        } else {
            let off = self.b.ins().ishl_imm_u(len, 3);
            let at = self.b.ins().iadd(ptr, off);
            for (k, &v) in values.iter().enumerate() {
                self.b.ins().store(MemFlagsData::new().with_notrap(), v, at, (k as i64 * VEC_LANES * 8) as i32);
            }
        }
    }

    /// The low byte of each lane of four `i64x2` (lanes in order) as one
    /// `i64`: eight bytes, the first lane lowest. Each vector moves its two
    /// bytes into place on its own (a `swizzle`, `pshufb` on x64; a
    /// two-operand `shuffle` may take AVX-512, which valgrind cannot run).
    fn vec_pack_bytes(&mut self, v: &[cranelift_codegen::ir::Value]) -> cranelift_codegen::ir::Value {
        let mut out = None;
        for (k, &x) in v.iter().enumerate() {
            let b = self.vcoerce(x, types::I8X16);
            // Bytes 0 and 8 to `2k` and `2k + 1`; an index past 15 is a 0.
            let mut lanes = [0x80u8; 16];
            lanes[2 * k] = 0;
            lanes[2 * k + 1] = 8;
            let c = self.b.func.dfg.constants.insert(cranelift_codegen::ir::ConstantData::from(lanes.to_vec()));
            let mask = self.b.ins().vconst(types::I8X16, c);
            let y = self.b.ins().swizzle(b, mask);
            out = Some(match out {
                None => y,
                Some(o) => self.b.ins().bor(o, y),
            });
        }
        let z = self.vcoerce(out.expect("four vectors"), types::I64X2);
        self.b.ins().extractlane(z, 0)
    }

    /// In front of a native lane loop over bytes: each source's bytes
    /// ([`Self::stream_bytes`], a string or packed bytes) into
    /// [`Self::lane_bytes`], and a check that every read of every lane
    /// `lo .. hi` is in them, or branch to `fail`.
    fn bytes_guard(&mut self, f: &MirFunc, reads: &[(ValueId, ValueId, bool)], lo: cranelift_codegen::ir::Value, hi: cranelift_codegen::ir::Value, fail: Block) {
        self.lane_bytes.clear();
        let mut all = self.b.ins().iconst(types::I8, 1);
        let mut seen = HashMap::new();
        for &(array, off, _) in reads {
            let (_, len) = match seen.get(&array) {
                Some(&x) => x,
                None => {
                    let (p, n, known) = self.stream_bytes(f, array);
                    let ok = self.b.ins().icmp_imm_s(IntCC::NotEqual, known, 0);
                    all = self.b.ins().band(all, ok);
                    self.lane_bytes.insert(array, p);
                    seen.insert(array, (p, n));
                    (p, n)
                }
            };
            let o = self.get(f, off);
            let o = self.coerce(o, types::I64);
            let (first, of1) = self.b.ins().sadd_overflow(lo, o);
            let (last, of2) = self.b.ins().sadd_overflow(hi, o);
            let of = self.b.ins().bor(of1, of2);
            let fine = self.b.ins().icmp_imm_s(IntCC::Equal, of, 0);
            let ge = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, first, 0);
            let le = self.b.ins().icmp(IntCC::SignedLessThanOrEqual, last, len);
            let x = self.b.ins().band(fine, ge);
            let x = self.b.ins().band(x, le);
            all = self.b.ins().band(all, x);
        }
        let go = self.b.create_block();
        self.b.ins().brif(all, go, &[], fail, &[]);
        self.b.switch_to_block(go);
    }

    /// Whether `e` lowers to native lane code over bytes: int lanes only,
    /// every load an int load (of a byte source), the kinds [`Self::lane`] knows.
    fn lanes_native_bytes(&self, e: &compiler::native_mir::VecExpr, depth: u32) -> bool {
        use common::dense;
        use compiler::native_mir::VecExpr;
        if depth > 32 || Self::lane_is_float(e) {
            return false;
        }
        match e {
            VecExpr::Splat { ty, .. } => *ty == dense::TY_I64,
            VecExpr::Index => true,
            VecExpr::Load { ty, .. } => *ty == dense::TY_I64,
            VecExpr::Bin { lhs, rhs, .. } => self.lanes_native_shape(e) && self.lanes_native_bytes(lhs, depth + 1) && self.lanes_native_bytes(rhs, depth + 1),
            VecExpr::Unary { src, .. } => self.lanes_native_shape(e) && self.lanes_native_bytes(src, depth + 1),
            VecExpr::Select { mask, then, els } => {
                self.lanes_native_bytes(mask, depth + 1) && self.lanes_native_bytes(then, depth + 1) && self.lanes_native_bytes(els, depth + 1)
            }
            VecExpr::Fma { .. } => false,
        }
    }

    /// Whether the lanes of `e` are floats.
    fn lane_is_float(e: &compiler::native_mir::VecExpr) -> bool {
        use common::{dense, simd};
        use compiler::native_mir::VecExpr;
        match e {
            VecExpr::Splat { ty, .. } | VecExpr::Load { ty, .. } | VecExpr::Fma { ty, .. } => *ty == dense::TY_F64,
            VecExpr::Index => false,
            VecExpr::Bin { kind, .. } => matches!(*kind, simd::FADD64 | simd::FSUB64 | simd::FMUL64 | simd::FDIV64),
            VecExpr::Unary { kind, .. } => matches!(*kind, simd::FNEG | simd::I2F),
            VecExpr::Select { then, .. } => Self::lane_is_float(then),
        }
    }

    /// Whether [`Self::lane`] knows the op of `e` itself (a `Bin` or `Unary`
    /// kind), whatever its operands.
    fn lanes_native_shape(&self, e: &compiler::native_mir::VecExpr) -> bool {
        use common::simd;
        use compiler::native_mir::VecExpr;
        match e {
            VecExpr::Bin { kind, .. } => {
                matches!(
                    *kind,
                    simd::IADD64 | simd::ISUB64 | simd::IMUL64 | simd::IDIV64 | simd::FADD64 | simd::FSUB64 | simd::FMUL64 | simd::FDIV64 | simd::IAND64 | simd::IOR64 | simd::IXOR64
                ) || (simd::ICMP..simd::ICMP + 6).contains(kind)
                    || (simd::FCMP..simd::FCMP + 6).contains(kind)
            }
            VecExpr::Unary { kind, .. } => matches!(*kind, simd::INEG | simd::FNEG | simd::I2F | simd::F2I),
            _ => true,
        }
    }

    /// In front of a native lane loop over lanes `lo .. hi`: take each
    /// array's view and check that every access `(array, off, store)` of
    /// every lane is in it (a store's through a plain view), or branch to
    /// `fail`. The loop's body then reads and stores in place unchecked.
    fn lanes_guard(&mut self, f: &MirFunc, refs: &[(ValueId, ValueId, bool)], lo: cranelift_codegen::ir::Value, hi: cranelift_codegen::ir::Value, fail: Block) {
        let mut seen = HashSet::new();
        for &(array, _, _) in refs {
            if seen.insert(array) {
                self.ensure_view(f, array);
            }
        }
        let mut all = self.b.ins().iconst(types::I8, 1);
        for &(array, off, store) in refs {
            let (ok, _, len) = self.views[&array];
            let ok = self.b.use_var(ok);
            let len = self.b.use_var(len);
            let has = if store {
                self.b.ins().icmp_imm_s(IntCC::Equal, ok, machine::VIEW_PLAIN as i64)
            } else {
                self.b.ins().icmp_imm_s(IntCC::NotEqual, ok, 0)
            };
            let o = self.get(f, off);
            let o = self.coerce(o, types::I64);
            let (first, of1) = self.b.ins().sadd_overflow(lo, o);
            let (last, of2) = self.b.ins().sadd_overflow(hi, o);
            let of = self.b.ins().bor(of1, of2);
            let fine = self.b.ins().icmp_imm_s(IntCC::Equal, of, 0);
            let ge = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, first, 0);
            let le = self.b.ins().icmp(IntCC::SignedLessThanOrEqual, last, len);
            let a = self.b.ins().band(has, fine);
            let b = self.b.ins().band(ge, le);
            let c = self.b.ins().band(a, b);
            all = self.b.ins().band(all, c);
        }
        let go = self.b.create_block();
        self.b.ins().brif(all, go, &[], fail, &[]);
        self.b.switch_to_block(go);
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
            VecExpr::Load { array, off, .. } if self.lanes_checked && self.lane_bytes.contains_key(array) => {
                let o = self.get(f, *off);
                let o = int(self, o);
                let i = self.b.ins().iadd(p, o);
                let addr = self.b.ins().iadd(self.lane_bytes[array], i);
                self.b.ins().uload8(types::I64, MemFlagsData::new().with_notrap(), addr, 0)
            }
            VecExpr::Load { array, off, ty } if self.lanes_checked => {
                let o = self.get(f, *off);
                let o = int(self, o);
                let i = self.b.ins().iadd(p, o);
                let at = self.element(*array, i);
                let w = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
                if *ty == dense::TY_F64 { flt(self, w) } else { w }
            }
            VecExpr::Load { array, off, ty } => {
                let o = self.get(f, *off);
                let o = int(self, o);
                let (i, of) = self.b.ins().sadd_overflow(p, o);
                self.leave_if(of, slow);
                let (fast, out, _join) = self.in_view(f, *array, i, false);
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

    /// Whether every lane of `e` is `0` or `1` (a compare, or bit logic of
    /// such lanes), so a product with it is a mask and cannot overflow.
    fn lane_is_01(e: &compiler::native_mir::VecExpr) -> bool {
        use common::simd;
        use compiler::native_mir::VecExpr;
        match e {
            VecExpr::Bin { kind, lhs, rhs } => {
                (simd::ICMP..simd::ICMP + 6).contains(kind)
                    || (simd::FCMP..simd::FCMP + 6).contains(kind)
                    || (matches!(*kind, simd::IAND64 | simd::IOR64 | simd::IXOR64) && Self::lane_is_01(lhs) && Self::lane_is_01(rhs))
            }
            _ => false,
        }
    }

    /// Rough machine instructions per lane of `e`: run one lane at a time
    /// ([`Self::lane`]) and on vectors ([`Self::vlane`]). An op with no
    /// vector form runs per lane inside the vector and pays for taking its
    /// lanes out and back in; an int add checks overflow by bit logic
    /// instead of a flag. Invariants cost nothing in the loop.
    fn lane_cost(e: &compiler::native_mir::VecExpr) -> (f32, f32) {
        use common::simd;
        use compiler::native_mir::VecExpr;
        // Taking two lanes out of a vector and one back.
        const PER_LANE: f32 = 3.0;
        let (s, v) = match e {
            VecExpr::Splat { .. } => (0.0, 0.0),
            VecExpr::Index => (0.0, 1.0),
            VecExpr::Load { .. } => (1.0, 0.5),
            VecExpr::Bin { kind, lhs, rhs } => {
                let (a, b) = (Self::lane_cost(lhs), Self::lane_cost(rhs));
                let own = match *kind {
                    simd::FADD64 | simd::FSUB64 | simd::FMUL64 | simd::IAND64 | simd::IOR64 | simd::IXOR64 => (1.0, 0.5),
                    simd::FDIV64 => (4.0, 2.0),
                    simd::IADD64 | simd::ISUB64 => (2.0, 2.5),
                    simd::IMUL64 if Self::lane_is_01(lhs) || Self::lane_is_01(rhs) => (2.0, 1.0),
                    simd::IMUL64 => (2.0, 2.0 + PER_LANE),
                    simd::IDIV64 => (8.0, 8.0 + PER_LANE),
                    _ => (2.0, 1.0),
                };
                (a.0 + b.0 + own.0, a.1 + b.1 + own.1)
            }
            VecExpr::Unary { kind, src } => {
                let a = Self::lane_cost(src);
                let own = match *kind {
                    simd::INEG => (2.0, 1.5),
                    simd::FNEG => (1.0, 0.5),
                    simd::I2F => (1.0, 1.5),
                    _ => (4.0, 4.0 + PER_LANE),
                };
                (a.0 + own.0, a.1 + own.1)
            }
            VecExpr::Select { mask, then, els } => {
                let (m, t, x) = (Self::lane_cost(mask), Self::lane_cost(then), Self::lane_cost(els));
                (m.0 + t.0 + x.0 + 2.0, m.1 + t.1 + x.1 + 2.0)
            }
            VecExpr::Fma { ty, a, b, c } => {
                let (x, y, z) = (Self::lane_cost(a), Self::lane_cost(b), Self::lane_cost(c));
                let own = if *ty == common::dense::TY_F64 { (2.0, 1.0) } else { (4.0, 4.5 + PER_LANE) };
                (x.0 + y.0 + z.0 + own.0, x.1 + y.1 + z.1 + own.1)
            }
        };
        (s, v)
    }

    /// Whether a native lane loop computing `values` (and folding with
    /// `fold`: its own cost per lane, one lane at a time and on vectors)
    /// is cheaper on vectors: their loop's own work is shared by a block's
    /// [`VEC_BLOCK`] lanes, a scalar loop pays it per lane.
    fn vectors_pay<'e>(values: impl IntoIterator<Item = &'e compiler::native_mir::VecExpr>, fold: (f32, f32)) -> bool {
        // Compare, branch and step of the index, per iteration.
        const LOOP: f32 = 4.0;
        let (mut s, mut v) = (LOOP + fold.0, LOOP / VEC_BLOCK as f32 + fold.1);
        for e in values {
            let (a, b) = Self::lane_cost(e);
            s += a;
            v += b;
        }
        v < s
    }

    /// The vector type a lane value of scalar type `ty` takes: two lanes
    /// of 64 bits ([`VEC_LANES`]).
    fn vec_ty(ty: Type) -> Type {
        if ty == types::F64 { types::F64X2 } else { types::I64X2 }
    }

    /// `x` as vector type `want` (the same bits, lane by lane).
    fn vcoerce(&mut self, x: cranelift_codegen::ir::Value, want: Type) -> cranelift_codegen::ir::Value {
        if self.b.func.dfg.value_type(x) == want {
            x
        } else {
            let flags = MemFlagsData::new().with_endianness(cranelift_codegen::ir::Endianness::Little);
            self.b.ins().bitcast(want, flags, x)
        }
    }

    /// An `i64x2` of `x` in both lanes.
    fn vsplat_i(&mut self, x: i64) -> cranelift_codegen::ir::Value {
        let c = self.b.ins().iconst(types::I64, x);
        self.b.ins().splat(types::I64X2, c)
    }

    /// `op` on each lane of vectors `a` and `c` as scalars of type
    /// `lane` (`c` is ignored by a unary `op`), and the vector of its
    /// results (of type `out`). `op` may flag its lane (an `i8`, non-zero:
    /// it would trap), which goes into `bad`.
    #[allow(clippy::type_complexity)]
    fn per_lane(
        &mut self,
        a: cranelift_codegen::ir::Value,
        c: cranelift_codegen::ir::Value,
        lane: Type,
        out: Type,
        bad: &mut VecBad,
        op: &dyn Fn(
            &mut Self,
            cranelift_codegen::ir::Value,
            cranelift_codegen::ir::Value,
        ) -> (cranelift_codegen::ir::Value, Option<cranelift_codegen::ir::Value>),
    ) -> cranelift_codegen::ir::Value {
        let vin = Self::vec_ty(lane);
        let a = self.vcoerce(a, vin);
        let c = self.vcoerce(c, vin);
        let mut v = None;
        for k in 0..VEC_LANES as u8 {
            let x = self.b.ins().extractlane(a, k);
            let y = self.b.ins().extractlane(c, k);
            let (r, flag) = op(self, x, y);
            if let Some(flag) = flag {
                bad.scalar(self, flag);
            }
            let r = self.coerce(r, out);
            v = Some(match v {
                None => self.b.ins().scalar_to_vector(Self::vec_ty(out), r),
                Some(v) => self.b.ins().insertlane(v, r, k),
            });
        }
        v.expect("two lanes")
    }

    /// Lanes `p`, `p + 1` of `e` as one vector (`i64x2` or `f64x2`), every
    /// load in place (after [`Self::lanes_guard`]). A lane that would trap
    /// is not taken: it is flagged in `bad`, and the vector holds some value
    /// there (a division by zero divides by `1`), so the caller runs those
    /// lanes again with [`Self::lane`], which traps as the scalar loop does.
    fn vlane(&mut self, f: &MirFunc, e: &compiler::native_mir::VecExpr, at: VecAt, bad: &mut VecBad) -> cranelift_codegen::ir::Value {
        use common::{dense, simd};
        use compiler::native_mir::VecExpr;
        let iv = |me: &mut Self, v| me.vcoerce(v, types::I64X2);
        let fv = |me: &mut Self, v| me.vcoerce(v, types::F64X2);
        match e {
            VecExpr::Splat { v, ty } => {
                let x = self.get(f, *v);
                let lane = if *ty == dense::TY_F64 { types::F64 } else { types::I64 };
                let x = self.coerce(x, lane);
                self.b.ins().splat(Self::vec_ty(lane), x)
            }
            VecExpr::Index => {
                let p = at.p;
                let v = self.b.ins().scalar_to_vector(types::I64X2, p);
                let p1 = self.b.ins().iadd_imm_s(p, 1);
                self.b.ins().insertlane(v, p1, 1)
            }
            VecExpr::Load { array, off, .. } if self.lane_bytes.contains_key(array) => {
                // The block's bytes from `array`, read once and widened.
                if let Some(v) = self.vec_bytes.get(&(*array, *off)) {
                    return v[at.half];
                }
                let base = self.lane_bytes[array];
                let o = self.get(f, *off);
                let o = self.coerce(o, types::I64);
                let i = self.b.ins().iadd(at.p0, o);
                let addr = self.b.ins().iadd(base, i);
                let h = self.b.ins().uload8x8(MemFlagsData::new().with_notrap(), addr, 0);
                let lo = self.b.ins().uwiden_low(h);
                let hi = self.b.ins().uwiden_high(h);
                let mut v = Vec::with_capacity(4);
                for w in [lo, hi] {
                    v.push(self.b.ins().uwiden_low(w));
                    v.push(self.b.ins().uwiden_high(w));
                }
                let x = v[at.half];
                self.vec_bytes.insert((*array, *off), v);
                x
            }
            VecExpr::Load { array, off, ty } => {
                let o = self.get(f, *off);
                let o = self.coerce(o, types::I64);
                let i = self.b.ins().iadd(at.p, o);
                let addr = self.element(*array, i);
                let vt = if *ty == dense::TY_F64 { types::F64X2 } else { types::I64X2 };
                self.b.ins().load(vt, MemFlagsData::new().with_notrap(), addr, 0)
            }
            VecExpr::Bin { kind, lhs, rhs } => {
                // A product with a 0 / 1 lane is that lane as a mask.
                if *kind == simd::IMUL64 && (Self::lane_is_01(lhs) || Self::lane_is_01(rhs)) {
                    let (m, x) = if Self::lane_is_01(lhs) { (lhs, rhs) } else { (rhs, lhs) };
                    let m = self.vlane(f, m, at, bad);
                    let x = self.vlane(f, x, at, bad);
                    let (m, x) = (iv(self, m), iv(self, x));
                    let full = self.b.ins().ineg(m);
                    return self.b.ins().band(x, full);
                }
                let a = self.vlane(f, lhs, at, bad);
                let c = self.vlane(f, rhs, at, bad);
                match *kind {
                    simd::FADD64 | simd::FSUB64 | simd::FMUL64 | simd::FDIV64 => {
                        let (a, c) = (fv(self, a), fv(self, c));
                        match *kind {
                            simd::FADD64 => self.b.ins().fadd(a, c),
                            simd::FSUB64 => self.b.ins().fsub(a, c),
                            simd::FMUL64 => self.b.ins().fmul(a, c),
                            _ => self.b.ins().fdiv(a, c),
                        }
                    }
                    k if (simd::FCMP..simd::FCMP + 6).contains(&k) => {
                        let (a, c) = (fv(self, a), fv(self, c));
                        let m = self.vlane_cmp(k - simd::FCMP, true, a, c);
                        self.b.ins().ushr_imm_u(m, 63)
                    }
                    k => {
                        let (a, c) = (iv(self, a), iv(self, c));
                        match k {
                            // Signed overflow: the result's sign differs
                            // from both operands' (an add) or from the
                            // minuend's when the operands' differ (a subtract).
                            simd::IADD64 => {
                                let r = self.b.ins().iadd(a, c);
                                let x = self.b.ins().bxor(a, r);
                                let y = self.b.ins().bxor(c, r);
                                let of = self.b.ins().band(x, y);
                                bad.vector(self, of);
                                r
                            }
                            simd::ISUB64 => {
                                let r = self.b.ins().isub(a, c);
                                let x = self.b.ins().bxor(a, c);
                                let y = self.b.ins().bxor(a, r);
                                let of = self.b.ins().band(x, y);
                                bad.vector(self, of);
                                r
                            }
                            simd::IMUL64 => self.per_lane(a, c, types::I64, types::I64, bad, &|me, x, y| {
                                let (r, of) = me.b.ins().smul_overflow(x, y);
                                (r, Some(of))
                            }),
                            simd::IDIV64 => self.per_lane(a, c, types::I64, types::I64, bad, &|me, x, y| {
                                let z = me.b.ins().icmp_imm_s(IntCC::Equal, y, 0);
                                let m = me.b.ins().icmp_imm_s(IntCC::Equal, x, i64::MIN);
                                let n = me.b.ins().icmp_imm_s(IntCC::Equal, y, -1);
                                let mn = me.b.ins().band(m, n);
                                let trap = me.b.ins().bor(z, mn);
                                let one = me.b.ins().iconst(types::I64, 1);
                                let y = me.b.ins().select(trap, one, y);
                                (me.b.ins().sdiv(x, y), Some(trap))
                            }),
                            simd::IAND64 => self.b.ins().band(a, c),
                            simd::IOR64 => self.b.ins().bor(a, c),
                            simd::IXOR64 => self.b.ins().bxor(a, c),
                            k => {
                                let m = self.vlane_cmp(k - simd::ICMP, false, a, c);
                                self.b.ins().ushr_imm_u(m, 63)
                            }
                        }
                    }
                }
            }
            VecExpr::Unary { kind, src } => {
                let a = self.vlane(f, src, at, bad);
                match *kind {
                    simd::INEG => {
                        let a = iv(self, a);
                        let min = self.vsplat_i(i64::MIN);
                        let of = self.b.ins().icmp(IntCC::Equal, a, min);
                        bad.vector(self, of);
                        self.b.ins().ineg(a)
                    }
                    simd::FNEG => {
                        let a = fv(self, a);
                        self.b.ins().fneg(a)
                    }
                    simd::I2F => {
                        let a = iv(self, a);
                        self.b.ins().fcvt_from_sint(types::F64X2, a)
                    }
                    _ => self.per_lane(a, a, types::F64, types::I64, bad, &|me, x, _| (me.b.ins().fcvt_to_sint_sat(types::I64, x), None)),
                }
            }
            VecExpr::Select { mask, then, els } => {
                let m = self.vlane(f, mask, at, bad);
                let m = iv(self, m);
                let t = self.vlane(f, then, at, bad);
                let e = self.vlane(f, els, at, bad);
                let ty = self.b.func.dfg.value_type(t);
                let zero = self.vsplat_i(0);
                let full = self.b.ins().icmp(IntCC::NotEqual, m, zero);
                let (t, e) = (iv(self, t), iv(self, e));
                let r = self.b.ins().bitselect(full, t, e);
                self.vcoerce(r, ty)
            }
            VecExpr::Fma { ty, a, b, c } => {
                let x = self.vlane(f, a, at, bad);
                let y = self.vlane(f, b, at, bad);
                let z = self.vlane(f, c, at, bad);
                if *ty == dense::TY_F64 {
                    let (x, y, z) = (fv(self, x), fv(self, y), fv(self, z));
                    let m = self.b.ins().fmul(x, y);
                    self.b.ins().fadd(m, z)
                } else {
                    let m = self.per_lane(x, y, types::I64, types::I64, bad, &|me, x, y| {
                        let (r, of) = me.b.ins().smul_overflow(x, y);
                        (r, Some(of))
                    });
                    let z = iv(self, z);
                    let r = self.b.ins().iadd(m, z);
                    let u = self.b.ins().bxor(m, r);
                    let w = self.b.ins().bxor(z, r);
                    let of = self.b.ins().band(u, w);
                    bad.vector(self, of);
                    r
                }
            }
        }
    }

    /// [`Self::lane_cmp`] on vectors: all-ones lanes where `a pred b`.
    fn vlane_cmp(&mut self, pred: u8, float: bool, a: cranelift_codegen::ir::Value, b: cranelift_codegen::ir::Value) -> cranelift_codegen::ir::Value {
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
            let m = self.b.ins().fcmp(cc, a, b);
            self.vcoerce(m, types::I64X2)
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
        self.vector_lanes_at(f, inst, None)
    }

    /// [`Self::vector_lanes`] from lane `at` instead of the op's `from`.
    fn vector_lanes_at(&mut self, f: &MirFunc, inst: &MirInst, at: Option<cranelift_codegen::ir::Value>) -> (cranelift_codegen::ir::Value, cranelift_codegen::ir::StackSlot) {
        self.vector_lanes_from(f, inst, at, None)
    }

    /// [`Self::vector_lanes_at`] with slot 0 starting at `acc` (a word)
    /// instead of the op's start value: a reduce resumed with the fold so
    /// far.
    fn vector_lanes_from(
        &mut self,
        f: &MirFunc,
        inst: &MirInst,
        at: Option<cranelift_codegen::ir::Value>,
        acc: Option<cranelift_codegen::ir::Value>,
    ) -> (cranelift_codegen::ir::Value, cranelift_codegen::ir::StackSlot) {
        let op = compiler::native_mir::vector_op(inst).expect("a vector op checked at planning");
        let n = op.inputs.len() + 2;
        let frame = self.b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
            (n * 8) as u32,
            3,
        ));
        let zero = self.b.ins().iconst(types::I64, 0);
        let start = match (acc, op.start) {
            (Some(a), _) => a,
            (None, Some(v)) => {
                let x = self.get(f, v);
                self.coerce(x, types::I64)
            }
            (None, None) => zero,
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
        let lo = match at {
            Some(at) => at,
            None => {
                let lo = self.get(f, lo);
                self.coerce(lo, types::I64)
            }
        };
        let hi = self.get(f, hi);
        let hi = self.coerce(hi, types::I64);
        let kind = self.b.ins().iconst(types::I64, kind);
        let flags = self.b.ins().iconst(types::I64, i64::from(op.flags));
        let body = self.b.ins().stack_addr(types::I64, code, 0);
        let nbody = self.b.ins().iconst(types::I64, op.body.len() as i64);
        let slots = self.b.ins().stack_addr(types::I64, frame, 0);
        let nslots = self.b.ins().iconst(types::I64, n as i64);
        let call = self.call_checked(self.vector, &[self.ctx, kind, flags, body, nbody, slots, nslots, lo, hi]);
        let w = self.b.inst_results(call)[0];
        (w, frame)
    }

    /// Call runtime helper `func`, then leave through the bail block when
    /// it set the context's bail flag (it panicked or refused).
    fn call_checked(&mut self, func: cranelift_codegen::ir::FuncRef, args: &[cranelift_codegen::ir::Value]) -> cranelift_codegen::ir::Inst {
        let call = self.b.ins().call(func, args);
        self.bail_on_helper();
        call
    }

    /// Name this generator frame's counted homes to the yield that comes
    /// next ([`machine::native::NativeCtx::gen_homes`]), so the cycle
    /// collector sees what the frame owns while it is suspended.
    fn note_gen_frame(&mut self, f: &MirFunc) {
        let Some(mask) = f.counted_homes() else { return };
        let homes = match self.homes_slot {
            Some(slot) => self.b.ins().stack_addr(types::I64, slot, 0),
            None if mask == 0 => self.b.ins().iconst(types::I64, machine::native::GEN_FRAME_EMPTY as i64),
            None => return,
        };
        let mask = self.b.ins().iconst(types::I64, mask as i64);
        self.b.ins().store(MemFlagsData::trusted(), homes, self.ctx, NATIVE_CTX_GEN_HOMES);
        self.b.ins().store(MemFlagsData::trusted(), mask, self.ctx, NATIVE_CTX_GEN_MASK);
    }

    /// Leave through the bail block when the last helper set the
    /// context's bail flag.
    fn bail_on_helper(&mut self) {
        let flag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_BAIL);
        self.bail_if(flag, TRAP_HELPER);
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
        let s = match (self.body_id, self.cur_loc) {
            // Without a location the site still names its function, as a
            // runtime frame without one does.
            (Some(body_id), loc) => {
                let loc = loc.unwrap_or(common::DebugLoc::unknown());
                self.sites.push(NativeSite { body_id, loc });
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
        self.root_handles = root_handles(f);
        self.in_range = in_range_steps(f);
        self.payload_views.clear();
        for s in payload_scrutinees(f) {
            let vars = (self.b.declare_var(types::I64), self.b.declare_var(types::I64));
            let zero = self.b.ins().iconst(types::I64, 0);
            self.b.def_var(vars.0, zero);
            self.b.def_var(vars.1, zero);
            self.payload_views.insert(s, vars);
        }
        self.product_views.clear();
        for o in field_objects(f) {
            let vars = (self.b.declare_var(types::I64), self.b.declare_var(types::I64));
            let zero = self.b.ins().iconst(types::I64, 0);
            self.b.def_var(vars.0, zero);
            self.b.def_var(vars.1, zero);
            self.product_views.insert(o, vars);
        }
        for blk in &f.blocks {
            for inst in &blk.insts {
                let read: Vec<ValueId> = match inst {
                    MirInst::HostInvoke { native_id, args, .. } if *native_id == common::STRING_BYTE_AT_ID && args.len() == 2 => vec![args[0]],
                    MirInst::HostInvoke { dest_hi: None, native_id, layout, args, .. } if slice_in_place_fits(*native_id, *layout, args) => vec![args[0]],
                    MirInst::ByteScan { streams, .. } => streams.iter().map(|&(s, _)| s).collect(),
                    _ => Vec::new(),
                };
                for s in read {
                    if self.str_views.contains_key(&s) {
                        continue;
                    }
                    let vars = (self.b.declare_var(types::I64), self.b.declare_var(types::I64), self.b.declare_var(types::I64));
                    let zero = self.b.ins().iconst(types::I64, 0);
                    for v in [vars.0, vars.1, vars.2] {
                        self.b.def_var(v, zero);
                    }
                    self.str_views.insert(s, vars);
                }
                let lane = match inst {
                    MirInst::LaneIndex { array, dest_more, .. } => Some((*array, 2 + dest_more.len())),
                    MirInst::LaneStore { array, more, .. } => {
                        self.stored.insert(*array);
                        Some((*array, 2 + more.len()))
                    }
                    _ => None,
                };
                if let Some((array, k)) = lane
                    && !self.lane_views.contains_key(&array)
                {
                    if k != 2 {
                        self.lane_words.insert(array, k as u8);
                    }
                    let vars = (self.b.declare_var(types::I64), self.b.declare_var(types::I64), self.b.declare_var(types::I64));
                    let zero = self.b.ins().iconst(types::I64, 0);
                    for v in [vars.0, vars.1, vars.2] {
                        self.b.def_var(v, zero);
                    }
                    self.lane_views.insert(array, vars);
                }
                // A word `VecLoop`'s arrays: its native loop reads and
                // stores in place (`native_reduce`, `native_store`).
                if let MirInst::VecLoop { kind, .. } = inst {
                    let mut arrays = Vec::new();
                    vec_loop_arrays(kind, &mut arrays);
                    // A push's or a store's sources as bytes too
                    // (`native_push`, `native_store`). Only those it reads:
                    // each view is one more variable every op that may move
                    // storage sets again.
                    if let compiler::native_mir::VecLoopKind::Push { .. } | compiler::native_mir::VecLoopKind::Store { .. } = kind {
                        for &(array, store) in &arrays {
                            if !store && !self.str_views.contains_key(&array) {
                                let vars = (self.b.declare_var(types::I64), self.b.declare_var(types::I64), self.b.declare_var(types::I64));
                                let zero = self.b.ins().iconst(types::I64, 0);
                                for v in [vars.0, vars.1, vars.2] {
                                    self.b.def_var(v, zero);
                                }
                                self.str_views.insert(array, vars);
                            }
                        }
                    }
                    for (array, store) in arrays {
                        if store {
                            self.stored.insert(array);
                        }
                        if !self.views.contains_key(&array) {
                            let vars = (self.b.declare_var(types::I64), self.b.declare_var(types::I64), self.b.declare_var(types::I64));
                            self.views.insert(array, vars);
                        }
                    }
                }
                let array = match inst {
                    MirInst::Index { array, .. } | MirInst::ArrayLen { array, .. } | MirInst::CowIndex { array, .. } => *array,
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
        if gc {
            self.safepoint();
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
        // A loop that may run forever with no call in it polls the stack
        // limit at its header as an entry does, so a cancelled
        // speculative job stops there too (#2442).
        let spins = if self.body_index.is_some_and(|i| self.plan.speculative.contains(&i)) {
            compiler::native_mir::spin_headers(f)
        } else {
            HashSet::new()
        };
        let first = self.blocks[&f.entry];
        self.b.ins().jump(first, &[]);
        let viewed: HashSet<ValueId> = self.views.keys().copied().collect();
        let dropped_in = views_dropped_in(f, &viewed);

        for blk in &f.blocks {
            let cl = self.blocks[&blk.id];
            self.b.switch_to_block(cl);
            self.block_views.clear();
            self.maybe_dropped = dropped_in.get(&blk.id).cloned().unwrap_or_default();
            if spins.contains(&blk.id) {
                let sp = self.b.ins().get_stack_pointer(types::I64);
                // An atomic load: another thread writes the limit, and a
                // plain one with no store in the loop is the entry's
                // load again to Cranelift's alias analysis.
                let at = self.b.ins().iadd_imm_s(self.ctx, i64::from(NATIVE_CTX_STACK_LIMIT));
                let limit = self.b.ins().atomic_load(types::I64, MemFlagsData::trusted(), at);
                let low = self.b.ins().icmp(IntCC::UnsignedLessThan, sp, limit);
                self.at(None);
                self.bail_if(low, TRAP_STACK);
            }
            if headers.contains(&blk.id) {
                self.at(None);
                self.safepoint();
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
                self.at(Some(f.op_loc(inst.dest())));
                self.inst(f, inst)?;
                drop_transfer(inst, &viewed, &mut self.maybe_dropped);
            }
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
            let n = self.b.ins().iconst(types::I64, f.homes.len() as i64);
            self.b.ins().call(self.defer, &[self.ctx, fp, at, n]);
        }
        let mut zeros = Vec::new();
        for t in std::iter::once(self.ret).chain(self.ret_hi).chain(self.ret_more.clone()) {
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
                let x = match op {
                    MirBinOp::Add if *ty == MirTy::I64 && self.in_range.contains(dest) => self.b.ins().iadd(a, c),
                    MirBinOp::Sub if *ty == MirTy::I64 && self.in_range.contains(dest) => self.b.ins().isub(a, c),
                    _ if *ty == MirTy::F64 => self.fbin(*op, a, c)?,
                    _ => self.ibin(*op, a, c)?,
                };
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
            // Its word view came along each edge ([`Self::edge_phis`]).
            MirInst::Phi { dest, .. } => self.lane_view_of(f, *dest),
            MirInst::CodePtr { dest, target } => {
                let pc = *self.plan.pcs.get(target).ok_or("code pointer of a body with no body id")?;
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
            MirInst::Call { dest, dest_hi, dest_more, target, args } => {
                // A `drop` called as a method: claim its finalizer first, and skip the call when it ran.
                let drop_pc = self
                    .by_label
                    .get(target)
                    .filter(|c| args.len() == 1 && self.plan.drops.contains(c))
                    .and_then(|_| self.plan.pcs.get(target).copied());
                let Some(pc) = drop_pc else {
                    return self.call(f, *dest, *dest_hi, dest_more, *target, args);
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
                for h in dest_hi.iter().chain(dest_more) {
                    self.set(f, *h, zero);
                }
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(go);
                self.call(f, *dest, *dest_hi, dest_more, *target, args)?;
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(join);
                self.view_of(f, *dest);
            }
            MirInst::CallIndirect { dest, callee, args, dicts, pair } => {
                let c = self.get(f, *callee);
                let c = self.coerce(c, types::I64);
                let p = self.words_slot(f, args);
                let n = self.b.ins().iconst(types::I64, args.len() as i64);
                let d = self.b.ins().iconst(types::I64, i64::from(*dicts));
                let roots = self.epoch_mark();
                let prev = self.push_call();
                // A plain function value enters its body here, as a direct
                // call does (`coil_native_fn_entry`); anything else goes
                // through the runtime.
                let values = args.len().saturating_sub(*dicts as usize);
                let nv = self.b.ins().iconst(types::I64, values as i64);
                // A two-word call's helper also writes whether it gave the
                // twin, past the words.
                let out_slot = self.b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    ((machine::native::FN_ENTRY_WORDS + usize::from(pair.is_some())) * 8) as u32,
                    3,
                ));
                let out = self.b.ins().stack_addr(types::I64, out_slot, 0);
                let helper = if pair.is_some() { self.fn_entry_pair } else { self.fn_entry };
                let ec = self.b.ins().call(helper, &[self.ctx, c, p, nv, out]);
                let entry = self.b.inst_results(ec)[0];
                let (direct, slow, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.append_block_param(join, types::I64);
                // A two-word result: the twin's words join `both`; a one-word
                // result reaches `join` and goes through `unpair`.
                let both = pair.map(|_| {
                    let both = self.b.create_block();
                    self.b.append_block_param(both, types::I64);
                    self.b.append_block_param(both, types::I64);
                    both
                });
                self.b.ins().brif(entry, direct, &[], slow, &[]);
                self.b.switch_to_block(direct);
                if let Some(both) = both {
                    let (twin, canon) = (self.b.create_block(), self.b.create_block());
                    let bit = self.b.ins().stack_load(types::I64, types::I64, out_slot, (machine::native::FN_ENTRY_WORDS * 8) as i32);
                    self.b.ins().brif(bit, twin, &[], canon, &[]);
                    self.b.switch_to_block(twin);
                    let call = self.b.ins().call_indirect(self.entry_sig, entry, &[self.ctx, out]);
                    let lo = self.b.inst_results(call)[0];
                    let hi = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, machine::native::NATIVE_CTX_RET_HI);
                    self.b.ins().jump(both, &[lo.into(), hi.into()]);
                    self.b.switch_to_block(canon);
                }
                let call = self.b.ins().call_indirect(self.entry_sig, entry, &[self.ctx, out]);
                let w = self.b.inst_results(call)[0];
                self.b.ins().jump(join, &[w.into()]);
                self.b.set_cold_block(slow);
                self.b.switch_to_block(slow);
                let call = self.b.ins().call(self.call_value, &[self.ctx, c, p, n, d]);
                let w = self.b.inst_results(call)[0];
                self.b.ins().jump(join, &[w.into()]);
                self.b.switch_to_block(join);
                let w = self.b.block_params(join)[0];
                let words = match (pair, both) {
                    (Some((_, unpair)), Some(both)) => {
                        // `unpair` reads the two words from the one.
                        self.bail_on_helper();
                        let callee = *self.by_label.get(unpair).ok_or("unknown unpair body")?;
                        let fref = *self.module_refs.get(&callee).ok_or("unpair body not compiled")?;
                        let sig = self.b.func.dfg.ext_funcs[fref].signature;
                        let params: Vec<Type> = self.b.func.dfg.signatures[sig].params.iter().map(|p| p.value_type).collect();
                        let mut vals = vec![self.ctx];
                        for (i, &want) in params[1..].iter().enumerate() {
                            let x = if i == 0 { w } else { self.b.ins().iconst(types::I64, 0) };
                            vals.push(self.coerce(x, want));
                        }
                        let call = self.b.ins().call(fref, &vals);
                        let results = self.b.inst_results(call).to_vec();
                        let (lo, hi) = (results[0], *results.get(1).ok_or("unpair body returns one word")?);
                        let (lo, hi) = (self.coerce(lo, types::I64), self.coerce(hi, types::I64));
                        self.b.ins().jump(both, &[lo.into(), hi.into()]);
                        self.b.switch_to_block(both);
                        let ps = self.b.block_params(both);
                        Some((ps[0], ps[1]))
                    }
                    _ => None,
                };
                self.pop_call(prev);
                self.epoch_check(roots);
                self.bail_on_helper();
                self.drop_views();
                match (words, pair) {
                    (Some((lo, hi)), Some((h, _))) => {
                        self.set(f, *dest, lo);
                        self.set(f, *h, hi);
                    }
                    _ => self.set_word(f, *dest, w),
                }
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
                let roots = self.epoch_mark();
                let prev = self.push_call();
                let call = self.b.ins().call(self.resume, &[self.ctx, h, sv, has]);
                self.pop_call(prev);
                let w = self.b.inst_results(call)[0];
                self.epoch_check(roots);
                self.bail_on_helper();
                self.drop_views();
                self.set_word(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::ByteScan { .. } | MirInst::VecLoop { .. } => {
                // A pure reduce compiles to a native loop; the rest runs the
                // runtime's `VScan` / `VLoop` kernels (`coil_native_vector`).
                if self.native_reduce(f, inst)? || self.native_store(f, inst)? || self.native_push(f, inst)? {
                    return Ok(());
                }
                if let (MirInst::ByteScan { dest, .. }, Some(w)) = (inst, self.scan_in_place(f, inst)) {
                    self.set(f, *dest, w);
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
                let roots = self.epoch_mark();
                let prev = self.push_call();
                let call = self.b.ins().call(self.ffi, &[self.ctx, o, x, p, n]);
                let w = self.b.inst_results(call)[0];
                self.pop_call(prev);
                self.epoch_check(roots);
                self.bail_on_helper();
                self.drop_views();
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::ExternCall { dest, sig, args } => {
                let w = match direct_extern(sig, self.c_abi).filter(|(p, r, va)| self.direct_externs && self.cb_call_ok(sig, args, p, *r, *va)) {
                    Some((params, ret, va)) => self.direct_extern_call(f, *dest, sig, args, &params, ret, va),
                    None => self.runtime_extern_call(f, *dest, sig, args),
                };
                self.drop_views();
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::Yield { dest, value, from: true, .. } => {
                // `yield from h`: resume `h` (sending what this generator's
                // resumer sent) and yield what it yields until it is done.
                let (lp, more, exit) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.append_block_param(lp, types::I64);
                let zero = self.b.ins().iconst(types::I64, 0);
                self.b.ins().jump(lp, &[zero.into()]);
                self.b.switch_to_block(lp);
                let send = self.b.block_params(lp)[0];
                let h = self.get(f, *value);
                let h = self.coerce(h, types::I64);
                let has = self.b.ins().iconst(types::I64, 1);
                let roots = self.epoch_mark();
                let prev = self.push_call();
                let call = self.b.ins().call(self.resume, &[self.ctx, h, send, has]);
                self.pop_call(prev);
                let w = self.b.inst_results(call)[0];
                self.epoch_check(roots);
                self.bail_on_helper();
                self.drop_views();
                let h = self.get(f, *value);
                let h = self.coerce(h, types::I64);
                let call = self.b.ins().call(self.coro_done, &[self.ctx, h]);
                let done = self.b.inst_results(call)[0];
                self.b.ins().brif(done, exit, &[], more, &[]);
                self.b.switch_to_block(more);
                self.note_gen_frame(f);
                let roots = self.epoch_mark();
                let prev = self.push_call();
                let call = self.b.ins().call(self.gen_yield, &[self.ctx, w]);
                self.pop_call(prev);
                let sent = self.b.inst_results(call)[0];
                self.epoch_check(roots);
                // A close unwinds from the yield (`coil_native_yield`).
                self.bail_on_helper();
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
                self.note_gen_frame(f);
                let roots = self.epoch_mark();
                let prev = self.push_call();
                let call = self.b.ins().call(self.gen_yield, &[self.ctx, v]);
                self.pop_call(prev);
                let w = self.b.inst_results(call)[0];
                self.epoch_check(roots);
                // A close unwinds from the yield (`coil_native_yield`).
                self.bail_on_helper();
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
                let (fast, slow, join) = self.in_view(f, *array, i, true);
                self.b.switch_to_block(fast);
                let at = self.element(*array, i);
                let w = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
                self.b.ins().jump(join, &[w.into()]);
                self.b.switch_to_block(slow);
                let call = self.call_checked(self.index, &[self.ctx, a, i]);
                let w = self.b.inst_results(call)[0];
                self.b.ins().jump(join, &[w.into()]);
                self.b.switch_to_block(join);
                let w = self.b.block_params(join)[0];
                self.set_word(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::ArrayLen { dest, array } => {
                let a = self.get(f, *array);
                let (ok_var, _, len) = self.views[array];
                let ok = self.b.use_var(ok_var);
                let ok = self.view_known(ok);
                let mut ok = self.b.ins().uextend(types::I64, ok);
                let mut n = self.b.use_var(len);
                // A lane array's element count, from its lane view.
                if let Some(&(lane_ok, _, lane_len)) = self.lane_views.get(array) {
                    let lane_ok = self.b.use_var(lane_ok);
                    let lane_ok = self.b.ins().icmp_imm_s(IntCC::Equal, lane_ok, 1);
                    let lane_ok = self.b.ins().uextend(types::I64, lane_ok);
                    let lane_len = self.b.use_var(lane_len);
                    n = self.b.ins().select(ok, n, lane_len);
                    ok = self.b.ins().bor(ok, lane_ok);
                }
                let fast = self.b.create_block();
                let slow = self.b.create_block();
                let join = self.b.create_block();
                self.b.append_block_param(join, types::I64);
                self.b.ins().brif(ok, fast, &[], slow, &[]);
                self.b.switch_to_block(fast);
                self.b.ins().jump(join, &[n.into()]);
                // A word view dropped since it was taken: take it again.
                self.b.switch_to_block(slow);
                let (refill, helper) = (self.b.create_block(), self.b.create_block());
                if !self.maybe_dropped.contains(array) {
                    self.b.ins().jump(helper, &[]);
                    self.b.switch_to_block(refill);
                    self.b.ins().jump(helper, &[]);
                } else {
                let state = self.b.use_var(ok_var);
                let unknown = self.b.ins().icmp_imm_s(IntCC::Equal, state, VIEW_UNKNOWN);
                self.b.ins().brif(unknown, refill, &[], helper, &[]);
                self.b.set_cold_block(refill);
                self.b.switch_to_block(refill);
                self.fill_view(f, *array);
                let state = self.b.use_var(ok_var);
                let known = self.view_known(state);
                let n = self.b.use_var(len);
                self.b.ins().brif(known, join, &[n.into()], helper, &[]);
                }
                self.b.switch_to_block(helper);
                let call = self.b.ins().call(self.len, &[a]);
                let w = self.b.inst_results(call)[0];
                self.b.ins().jump(join, &[w.into()]);
                self.b.switch_to_block(join);
                let w = self.b.block_params(join)[0];
                self.set(f, *dest, w);
            }
            MirInst::ProductLoad { dest, object, slot } => {
                let o = self.get(f, *object);
                let o = self.coerce(o, types::I64);
                let (fast, slow) = self.field_slot(*object, o, *slot, false);
                self.b.switch_to_block(slow);
                let args = self.bail_args(TRAP_HELPER);
                self.b.ins().jump(self.bail, &args);
                self.b.switch_to_block(fast);
                let at = self.b.block_params(fast)[0];
                let w = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
                self.set_word(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::StoreIndex { dest, array, index, value, scalar, .. } => {
                // A store goes in place in a plain view
                // when it can count the store itself: the new word gains an
                // owner and the old one loses one. A word that may or may
                // not be an object stays in place only when neither the new
                // nor the old one can be; the helper counts the rest.
                let a = self.get(f, *array);
                let i = self.get(f, *index);
                let v = self.get(f, *value);
                let v = self.coerce(v, types::I64);
                let vt = ty_of(f, *value);
                let (fast, slow, join) = self.in_view_as(f, *array, i, false, true);
                self.b.switch_to_block(fast);
                let at = self.element(*array, i);
                if counted_inline(vt) {
                    let old = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
                    self.b.ins().store(MemFlagsData::trusted(), v, at, 0);
                    self.emit_rc(v, false);
                    self.emit_rc(old, true);
                    self.b.ins().jump(join, &[]);
                } else if *scalar || matches!(vt, MirTy::F64 | MirTy::Bool) {
                    // A number element's old and new words are numbers.
                    self.b.ins().store(MemFlagsData::trusted(), v, at, 0);
                    self.b.ins().jump(join, &[]);
                } else {
                    let (obj, plain) = self.maybe_object(v);
                    self.b.switch_to_block(obj);
                    self.b.ins().jump(slow, &[]);
                    self.b.switch_to_block(plain);
                    let old = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
                    let (obj, plain) = self.maybe_object(old);
                    self.b.switch_to_block(obj);
                    self.b.ins().jump(slow, &[]);
                    self.b.switch_to_block(plain);
                    self.b.ins().store(MemFlagsData::trusted(), v, at, 0);
                    self.b.ins().jump(join, &[]);
                }
                self.b.switch_to_block(slow);
                self.call_checked(self.store_index, &[self.ctx, a, i, v]);
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(join);
                let v = self.get(f, *value);
                self.set(f, *dest, v);
            }
            MirInst::HeapFieldStore { dest, object, value, index, scalar } => {
                let o = self.get(f, *object);
                let o = self.coerce(o, types::I64);
                let v = self.get(f, *value);
                let v = self.coerce(v, types::I64);
                let join = self.b.create_block();
                let vt = ty_of(f, *value);
                // A run that keeps counts stores in place too when the word's
                // type says whether it is an object: the new word gains an
                // owner, the old one loses one (reference-count plan step 6).
                // A number field's old and new words are numbers (`scalar`).
                let inline = counted_inline(vt) || *scalar || matches!(vt, MirTy::F64 | MirTy::Bool);
                if machine::native::product_slots().inline_stores && inline {
                    // In place when the object is an instance.
                    let (fast, slow) = self.field_slot(*object, o, *index, true);
                    self.b.switch_to_block(fast);
                    let at = self.b.block_params(fast)[0];
                    if counted_inline(vt) {
                        let old = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
                        self.b.ins().store(MemFlagsData::trusted(), v, at, 0);
                        self.emit_rc(v, false);
                        self.emit_rc(old, true);
                    } else {
                        self.b.ins().store(MemFlagsData::trusted(), v, at, 0);
                    }
                    self.b.ins().jump(join, &[]);
                    self.b.switch_to_block(slow);
                }
                let i = self.b.ins().iconst(types::I64, i64::from(*index));
                self.call_checked(self.set_field, &[self.ctx, o, i, v]);
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(join);
                self.set(f, *dest, v);
            }
            MirInst::RcReuse { dest, src, moved } => {
                let s = self.get(f, *src);
                let s = self.coerce(s, types::I64);
                let t = MemFlagsData::trusted();
                let done = self.b.create_block();
                self.b.append_block_param(done, types::I64);
                let (helper, shared) = (self.b.create_block(), self.b.create_block());
                // An object someone else still owns loses this owner in
                // place, as `rc.dec` does, and keeps no slot: the helper is
                // for a last owner (or a steal epoch, an immortal).
                let (object, none) = self.maybe_object(s);
                self.b.switch_to_block(none);
                let zero = self.b.ins().iconst(types::I64, 0);
                self.b.ins().jump(done, &[zero.into()]);
                self.b.switch_to_block(object);
                let o = self.b.ins().band_imm_u(s, !1i64);
                let heap = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_HEAP);
                let lock = self.b.ins().load(types::I64, t, heap, machine::HEAP_ALLOC_LOCK_OFFSET);
                let count_b = self.b.create_block();
                self.b.ins().brif(lock, helper, &[], count_b, &[]);
                self.b.switch_to_block(count_b);
                let c = self.b.ins().uload32(t, o, machine::HEADER_COUNT_OFFSET);
                let (last, test) = (self.b.create_block(), self.b.create_block());
                let one = self.b.ins().icmp_imm_u(IntCC::UnsignedLessThanOrEqual, c, 1);
                self.b.ins().brif(one, last, &[], test, &[]);
                self.b.switch_to_block(test);
                let immortal = self.b.ins().icmp_imm_s(IntCC::Equal, c, i64::from(machine::COUNT_IMMORTAL));
                self.b.ins().brif(immortal, helper, &[], shared, &[]);
                // The last owner: the lean helper, which knows the word an
                // object of this heap and no epoch open.
                self.b.switch_to_block(last);
                let m = self.b.ins().iconst(types::I64, i64::from(*moved));
                let call = self.b.ins().call(self.rc_take, &[self.ctx, s, m]);
                let slot = self.b.inst_results(call)[0];
                self.b.ins().jump(done, &[slot.into()]);
                self.b.switch_to_block(shared);
                let n = self.b.ins().iadd_imm_s(c, -1);
                self.b.ins().istore32(t, n, o, machine::HEADER_COUNT_OFFSET);
                // The moved words gain the owners the body did not count.
                for i in 0..32u32 {
                    if *moved & (1 << i) == 0 {
                        continue;
                    }
                    let ty = f.blocks.iter().flat_map(|b| &b.insts).find_map(|inst| match inst {
                        MirInst::MatchPayload { dest, scrutinee, index } | MirInst::ProductLoad { dest, object: scrutinee, slot: index } if scrutinee == src && *index == i => Some(ty_of(f, *dest)),
                        _ => None,
                    });
                    let w = self.payload(f, *src, s, i);
                    if ty.is_some_and(counted_inline) {
                        self.emit_rc(w, false);
                    } else {
                        let (call, join) = self.maybe_object(w);
                        self.b.switch_to_block(call);
                        let zero = self.b.ins().iconst(types::I64, 0);
                        self.b.ins().call(self.rc, &[self.ctx, w, zero]);
                        self.b.ins().jump(join, &[]);
                        self.b.switch_to_block(join);
                    }
                }
                let zero = self.b.ins().iconst(types::I64, 0);
                self.b.ins().jump(done, &[zero.into()]);
                self.b.switch_to_block(helper);
                let moved = self.b.ins().iconst(types::I64, i64::from(*moved));
                let call = self.b.ins().call(self.rc_reuse, &[self.ctx, s, moved]);
                let slot = self.b.inst_results(call)[0];
                self.b.ins().jump(done, &[slot.into()]);
                self.b.switch_to_block(done);
                let slot = self.b.block_params(done)[0];
                self.set(f, *dest, slot);
            }
            MirInst::Alloc { dest, kind, elems, kinds } | MirInst::AllocAt { dest, kind, elems, kinds, .. } => {
                let words = elems;
                // A slot `RcReuse` kept (`0`: none).
                let reuse = match inst {
                    MirInst::AllocAt { slot: Some(slot), .. } => {
                        let s = self.get(f, *slot);
                        Some(self.coerce(s, types::I64))
                    }
                    _ => None,
                };
                // Payload words the frame's owner moves into (`0`: none).
                let owned = match inst {
                    MirInst::AllocAt { owned, .. } => *owned,
                    _ => 0,
                };
                // An opaque body's `int` words may be objects: their kinds
                // stay unknown.
                let packed = || {
                    common::pack_word_kinds(words.iter().map(|w| match ty_of(f, *w) {
                        MirTy::I64 | MirTy::MaybeObj if f.opaque_words => common::WORD_UNKNOWN,
                        t => word_kind(t),
                    }))
                };
                let AllocCall { code, n, extra, last } =
                    alloc_call(*kind, words.len(), *kinds, packed, |t| self.plan.pcs.get(&t).copied()).map_err(|_| {
                        if matches!(kind, MirAllocKind::Coro { .. }) {
                            "generator of a body with no body id"
                        } else {
                            "function value of a body with no body id"
                        }
                    })?;
                // A unit variant is one shared immortal object per tag: read
                // from the heap's table once it exists, without a call.
                let unit_join = match kind {
                    MirAllocKind::Enum { tag } if words.is_empty() && reuse.is_none() && (*tag as usize) < machine::UNIT_ENUM_FAST => {
                        let t = MemFlagsData::trusted();
                        let (have_heap, hit, slow, uj) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
                        self.b.append_block_param(uj, types::I64);
                        let heap = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_HEAP);
                        self.b.ins().brif(heap, have_heap, &[], slow, &[]);
                        self.b.switch_to_block(have_heap);
                        let at = machine::HEAP_UNIT_ENUMS_OFFSET + 8 * (*tag as i32);
                        let w = self.b.ins().load(types::I64, t, heap, at);
                        self.b.ins().brif(w, hit, &[], slow, &[]);
                        self.b.switch_to_block(hit);
                        self.b.ins().jump(uj, &[w.into()]);
                        self.b.set_cold_block(slow);
                        self.b.switch_to_block(slow);
                        Some(uj)
                    }
                    _ => None,
                };
                let inline = inline_shape(*kind, words.len(), extra).and_then(machine::native::inline_object);
                let join = inline.map(|obj| {
                    let join = self.b.create_block();
                    self.b.append_block_param(join, types::I64);
                    self.inline_alloc(f, &obj, *kind, words, extra, join, reuse, owned);
                    join
                });
                // The runtime's allocation takes the slot (or gives it back).
                if let Some(slot) = reuse {
                    self.b.ins().call(self.offer_slot, &[self.ctx, slot]);
                }
                let p = match last {
                    Some(w) => self.words_slot_with(f, words, Some(w as i64)),
                    None => self.words_slot(f, words),
                };
                let code = self.b.ins().iconst(types::I64, code as i64);
                let n = self.b.ins().iconst(types::I64, n as i64);
                let extra = self.b.ins().iconst(types::I64, extra as i64);
                let call = self.call_checked(self.alloc, &[self.ctx, code, p, n, extra]);
                let mut w = self.b.inst_results(call)[0];
                // The runtime counted every word: an owned one gives the
                // frame's owner back (the object holds one, so none frees).
                for (i, &e) in words.iter().enumerate() {
                    if i < 32 && owned & (1 << i) != 0 {
                        let x = self.get(f, e);
                        let x = self.coerce(x, types::I64);
                        if counted_inline(ty_of(f, e)) {
                            self.emit_rc(x, true);
                        } else {
                            let one = self.b.ins().iconst(types::I64, 1);
                            self.b.ins().call(self.rc, &[self.ctx, x, one]);
                        }
                    }
                }
                if let Some(join) = join {
                    self.b.ins().jump(join, &[w.into()]);
                    self.b.switch_to_block(join);
                    w = self.b.block_params(join)[0];
                }
                if let Some(uj) = unit_join {
                    self.b.ins().jump(uj, &[w.into()]);
                    self.b.switch_to_block(uj);
                    w = self.b.block_params(uj)[0];
                }
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::ArrayPush { dest, array, value } => {
                let a = self.get(f, *array);
                let v = self.get(f, *value);
                let v = self.coerce(v, types::I64);
                let l = machine::native::array_slots();
                if !l.inline_push {
                    let call = self.call_checked(self.array_push, &[self.ctx, a, v]);
                    let w = self.b.inst_results(call)[0];
                    self.drop_views();
                    self.set(f, *dest, w);
                    self.view_of(f, *dest);
                } else {
                    // In place when the array holds words and has room: the
                    // element goes after the last, the length grows by one and
                    // the element gains an owner, as the helper does. Otherwise
                    // (bytes, lanes, a full `Vec`) the helper, which may move
                    // the storage.
                    let t = MemFlagsData::trusted();
                    let (kind_b, words_b, room_b, fast, slow, join) =
                        (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
                    let (bytes_b, byte_fits, bytes_fast) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                    self.b.ins().brif(a, kind_b, &[], slow, &[]);
                    self.b.switch_to_block(kind_b);
                    let k = self.b.ins().uload8(types::I64, t, a, l.kind_at as i32);
                    let is = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(l.kind));
                    self.b.ins().brif(is, words_b, &[], slow, &[]);
                    self.b.switch_to_block(words_b);
                    let tag = self.b.ins().uload8(types::I64, t, a, l.data_at as i32);
                    let is = self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(l.words_tag));
                    self.b.ins().brif(is, room_b, &[], bytes_b, &[]);
                    // Packed bytes: a value that fits a byte goes in place
                    // when there is room. Byte storage has no word view and
                    // holds no references, so only the new name's view is
                    // taken again at its next access.
                    self.b.switch_to_block(bytes_b);
                    let is = self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(l.bytes_tag));
                    let small = self.b.ins().icmp_imm_u(IntCC::UnsignedLessThanOrEqual, v, 255);
                    let ok = self.b.ins().band(is, small);
                    self.b.ins().brif(ok, byte_fits, &[], slow, &[]);
                    self.b.switch_to_block(byte_fits);
                    let blen = self.b.ins().load(types::I64, t, a, l.bytes_len_at as i32);
                    let bcap = self.b.ins().load(types::I64, t, a, l.bytes_cap_at as i32);
                    let room = self.b.ins().icmp(IntCC::UnsignedLessThan, blen, bcap);
                    self.b.ins().brif(room, bytes_fast, &[], slow, &[]);
                    self.b.switch_to_block(bytes_fast);
                    let bptr = self.b.ins().load(types::I64, t, a, l.bytes_ptr_at as i32);
                    let at = self.b.ins().iadd(bptr, blen);
                    self.b.ins().istore8(t, v, at, 0);
                    let bn = self.b.ins().iadd_imm_s(blen, 1);
                    self.b.ins().store(t, bn, a, l.bytes_len_at as i32);
                    self.set(f, *dest, a);
                    if *dest != *array
                        && let Some(&(ok, _, _)) = self.views.get(dest)
                    {
                        let unknown = self.b.ins().iconst(types::I64, VIEW_UNKNOWN);
                        self.b.def_var(ok, unknown);
                    }
                    self.b.ins().jump(join, &[]);
                    self.b.switch_to_block(room_b);
                    let len = self.b.ins().load(types::I64, t, a, l.len_at as i32);
                    let cap = self.b.ins().load(types::I64, t, a, l.cap_at as i32);
                    let room = self.b.ins().icmp(IntCC::UnsignedLessThan, len, cap);
                    self.b.ins().brif(room, fast, &[], slow, &[]);
                    self.b.switch_to_block(fast);
                    let ptr = self.b.ins().load(types::I64, t, a, l.ptr_at as i32);
                    let off = self.b.ins().ishl_imm_u(len, 3);
                    let at = self.b.ins().iadd(ptr, off);
                    self.b.ins().store(t, v, at, 0);
                    let n = self.b.ins().iadd_imm_s(len, 1);
                    self.b.ins().store(t, n, a, l.len_at as i32);
                    let one = self.b.ins().iconst(types::I8, 1);
                    self.b.ins().store(t, one, a, l.refs_at as i32);
                    let vt = ty_of(f, *value);
                    if counted_inline(vt) {
                        self.emit_rc(v, false);
                    } else if !matches!(vt, MirTy::F64 | MirTy::Bool) {
                        // A word that may name an object: the runtime counts it
                        // by the array's element kind.
                        let (call, done) = self.maybe_object(v);
                        self.b.switch_to_block(call);
                        let zero = self.b.ins().iconst(types::I64, 0);
                        self.b.ins().call(self.rc, &[self.ctx, v, zero]);
                        self.b.ins().jump(done, &[]);
                        self.b.switch_to_block(done);
                    }
                    // The storage did not move: another array's view stays,
                    // and one of this array's storage (another name of it,
                    // with the old length) is taken again at its next access.
                    // Its own view is in hand.
                    let others: Vec<_> = self.views.iter().filter(|(id, _)| **id != *array && **id != *dest).map(|(_, v)| *v).collect();
                    let lanes: Vec<_> = self.lane_views.values().map(|v| (v.0, v.1, 0)).collect();
                    for (ok, vp, gone) in others.into_iter().map(|(o, p, _)| (o, p, VIEW_UNKNOWN)).chain(lanes) {
                        let p = self.b.use_var(vp);
                        let st = self.b.use_var(ok);
                        let same = self.b.ins().icmp(IntCC::Equal, p, ptr);
                        let unknown = self.b.ins().iconst(types::I64, gone);
                        let st = self.b.ins().select(same, unknown, st);
                        self.b.def_var(ok, st);
                    }
                    self.set(f, *dest, a);
                    for id in [*array, *dest] {
                        if let Some(&(ok, vp, vl)) = self.views.get(&id) {
                            let plain = self.b.ins().iconst(types::I64, machine::VIEW_PLAIN as i64);
                            self.b.def_var(ok, plain);
                            self.b.def_var(vp, ptr);
                            self.b.def_var(vl, n);
                        }
                    }
                    self.b.ins().jump(join, &[]);
                    self.b.set_cold_block(slow);
                    self.b.switch_to_block(slow);
                    let call = self.call_checked(self.array_push, &[self.ctx, a, v]);
                    let w = self.b.inst_results(call)[0];
                    self.drop_views();
                    self.set(f, *dest, w);
                    self.view_of(f, *dest);
                    self.b.ins().jump(join, &[]);
                    self.b.switch_to_block(join);
                }
            }
            MirInst::LaneIndex { dest, dest_hi, dest_more, array, index, shape } if !dest_more.is_empty() => {
                let a = self.get(f, *array);
                let i = self.get(f, *index);
                let i = self.coerce(i, types::I64);
                let dests: Vec<ValueId> = [*dest, *dest_hi].into_iter().chain(dest_more.iter().copied()).collect();
                let k = dests.len();
                // In place when the array is in lanes and `i` in bounds;
                // otherwise the helper (a boxed element, a trap).
                let (fast, slow) = self.in_lane_view(f, *array, i);
                let join = self.b.create_block();
                for _ in 0..k {
                    self.b.append_block_param(join, types::I64);
                }
                self.b.switch_to_block(fast);
                let at = self.lane_element(*array, i);
                let words: Vec<cranelift_codegen::ir::BlockArg> = (0..k).map(|n| self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 8 * n as i32).into()).collect();
                self.b.ins().jump(join, &words);
                self.b.switch_to_block(slow);
                let kk = self.b.ins().iconst(types::I64, k as i64);
                let out = self.b.ins().stack_addr(types::I64, self.view_slot, 0);
                let [_, nl, nh] = self.lane_shape(shape);
                self.call_checked(self.lane_index_wide, &[self.ctx, a, i, kk, out, nl, nh]);
                let words: Vec<cranelift_codegen::ir::BlockArg> = (0..k).map(|n| self.b.ins().stack_load(types::I64, types::I64, self.view_slot, 8 * n as i32).into()).collect();
                self.b.ins().jump(join, &words);
                self.b.switch_to_block(join);
                let words = self.b.block_params(join).to_vec();
                for (d, w) in dests.into_iter().zip(words) {
                    self.set_word(f, d, w);
                }
            }
            MirInst::LaneIndex { dest, dest_hi, array, index, .. } => {
                let a = self.get(f, *array);
                let i = self.get(f, *index);
                let i = self.coerce(i, types::I64);
                // In place when the array is in lanes and `i` in bounds;
                // otherwise the helper (a boxed element, a trap).
                let (fast, slow) = self.in_lane_view(f, *array, i);
                let join = self.b.create_block();
                self.b.append_block_param(join, types::I64);
                self.b.append_block_param(join, types::I64);
                self.b.switch_to_block(fast);
                let at = self.lane_element(*array, i);
                let lo = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
                let hi = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 8);
                self.b.ins().jump(join, &[lo.into(), hi.into()]);
                self.b.switch_to_block(slow);
                let call = self.call_checked(self.lane_index, &[self.ctx, a, i]);
                let lo = self.b.inst_results(call)[0];
                let hi = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, machine::native::NATIVE_CTX_RET_HI);
                self.b.ins().jump(join, &[lo.into(), hi.into()]);
                self.b.switch_to_block(join);
                let (lo, hi) = (self.b.block_params(join)[0], self.b.block_params(join)[1]);
                self.set_word(f, *dest, lo);
                self.set_word(f, *dest_hi, hi);
            }
            MirInst::LaneStore { dest, array, index, lo, hi, more, shape } if !more.is_empty() => {
                let a = self.get(f, *array);
                let i = self.get(f, *index);
                let i = self.coerce(i, types::I64);
                let mut words = Vec::with_capacity(2 + more.len());
                for v in [*lo, *hi].iter().chain(more) {
                    let w = self.get(f, *v);
                    words.push(self.coerce(w, types::I64));
                }
                // In place when the array is in lanes and `i` in bounds and
                // the words are numbers, so no count changes; a heap leaf's
                // store the helper counts.
                let (fast, slow) = self.in_lane_view(f, *array, i);
                let join = self.b.create_block();
                self.b.switch_to_block(fast);
                if shape.tag_mask == 0 {
                    let at = self.lane_element(*array, i);
                    for (n, &w) in words.iter().enumerate() {
                        self.b.ins().store(MemFlagsData::trusted(), w, at, 8 * n as i32);
                    }
                    self.b.ins().jump(join, &[]);
                } else {
                    self.b.ins().jump(slow, &[]);
                }
                self.b.switch_to_block(slow);
                let buf = self.lane_buffer(&words);
                let k = self.b.ins().iconst(types::I64, words.len() as i64);
                let [t, nl, nh] = self.lane_shape(shape);
                self.call_checked(self.lane_store_wide, &[self.ctx, a, i, buf, k, t, nl, nh]);
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(join);
                let one = self.b.ins().iconst(types::I64, 1);
                self.set(f, *dest, one);
            }
            MirInst::LaneStore { dest, array, index, lo, hi, shape, .. } => {
                let a = self.get(f, *array);
                let i = self.get(f, *index);
                let lo = self.get(f, *lo);
                let lo = self.coerce(lo, types::I64);
                let hi = self.get(f, *hi);
                let hi = self.coerce(hi, types::I64);
                let i = self.coerce(i, types::I64);
                let (fast, slow) = self.in_lane_view(f, *array, i);
                let join = self.b.create_block();
                self.b.switch_to_block(fast);
                // The helper counts the store.
                self.b.ins().jump(slow, &[]);
                self.b.switch_to_block(slow);
                let [t, p, u] = self.lane_shape(shape);
                self.call_checked(self.lane_store, &[self.ctx, a, i, lo, hi, t, p, u]);
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(join);
                let one = self.b.ins().iconst(types::I64, 1);
                self.set(f, *dest, one);
            }
            MirInst::LanePush { dest, array, lo, hi, more, shape } if !more.is_empty() => {
                let a = self.get(f, *array);
                let mut words = Vec::with_capacity(2 + more.len());
                for v in [*lo, *hi].iter().chain(more) {
                    let w = self.get(f, *v);
                    words.push(self.coerce(w, types::I64));
                }
                let buf = self.lane_buffer(&words);
                let k = self.b.ins().iconst(types::I64, words.len() as i64);
                let [t, nl, nh] = self.lane_shape(shape);
                let call = self.call_checked(self.lane_push_wide, &[self.ctx, a, buf, k, t, nl, nh]);
                let w = self.b.inst_results(call)[0];
                self.drop_views();
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::LanePush { dest, array, lo, hi, shape, .. } => {
                let a = self.get(f, *array);
                let lo = self.get(f, *lo);
                let lo = self.coerce(lo, types::I64);
                let hi = self.get(f, *hi);
                let hi = self.coerce(hi, types::I64);
                let [t, p, u] = self.lane_shape(shape);
                let call = self.call_checked(self.lane_push, &[self.ctx, a, lo, hi, t, p, u]);
                let w = self.b.inst_results(call)[0];
                self.drop_views();
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::LanesToWords { dest, array, shape } => {
                let a = self.get(f, *array);
                let [t, p, u] = self.lane_shape(shape);
                let call = self.call_checked(self.lanes_to_words, &[self.ctx, a, t, p, u]);
                let w = self.b.inst_results(call)[0];
                self.drop_views();
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::String { dest, idx } => {
                let i = self.b.ins().iconst(types::I64, i64::from(*idx));
                let call = self.call_checked(self.string, &[self.ctx, i]);
                let w = self.b.inst_results(call)[0];
                self.set(f, *dest, w);
            }
            // A pad's end: the runtime goes on unwinding once the pad returns.
            MirInst::HostInvoke { dest, native_id, .. } if *native_id == common::UNWIND_RESUME_ID => {
                if numeric(ty_of(f, *dest)) {
                    let zero = self.b.ins().iconst(types::I64, 0);
                    self.set(f, *dest, zero);
                }
            }
            MirInst::HostInvoke { dest, dest_hi: None, native_id, layout, args } if map_op(f, *native_id, args).is_some() => {
                let op = map_op(f, *native_id, args).expect("guarded");
                self.lower_map_op(f, op, *dest, *native_id, *layout, args);
            }
            MirInst::HostInvoke { dest, dest_hi: None, native_id, layout, args } if slice_in_place_fits(*native_id, *layout, args) && self.str_views.contains_key(&args[0]) => {
                // A short slice of a string whose bytes are in view: made
                // here (`slice_in_place`), else the host call.
                let join = self.slice_in_place(f, args);
                let w = self.host_call(f, *native_id, *layout, args);
                self.b.ins().jump(join, &[w.into()]);
                self.b.switch_to_block(join);
                let w = self.b.block_params(join)[0];
                if !keeps_views(*native_id) {
                    self.drop_views();
                }
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::HostInvoke { dest, dest_hi: None, native_id, layout, args } if string_from_bytes_in_place_fits(*native_id, *layout, args) => {
                let join = self.string_from_bytes_in_place(f, args[0]);
                let w = self.host_call(f, *native_id, *layout, args);
                self.b.ins().jump(join, &[w.into()]);
                self.b.switch_to_block(join);
                let w = self.b.block_params(join)[0];
                if !keeps_views(*native_id) {
                    self.drop_views();
                }
                self.set(f, *dest, w);
                self.view_of(f, *dest);
            }
            MirInst::HostInvoke { dest, dest_hi, native_id, layout, args } => {
                // `s[i]` on a string: the byte in place when `i` is in range,
                // else the host call (out of range, a boxed string).
                let byte_read = *native_id == common::STRING_BYTE_AT_ID && args.len() == 2 && self.str_views.contains_key(&args[0]);
                let join = if byte_read { Some(self.byte_in_place(f, args[0], args[1])) } else { None };
                let w = self.host_call(f, *native_id, *layout, args);
                // A two-word result's tag, which the runtime left in the
                // context (`HOST_ENUM_LAYOUT_PAIR`).
                if let Some(h) = dest_hi {
                    let tag = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, NATIVE_CTX_RET_HI);
                    self.set(f, *h, tag);
                }
                if let Some(join) = join {
                    // `string_byte_at` neither collects nor moves anything:
                    // every view holds.
                    self.b.ins().jump(join, &[w.into()]);
                    self.b.switch_to_block(join);
                    let w = self.b.block_params(join)[0];
                    self.set(f, *dest, w);
                } else {
                    if !keeps_views(*native_id) {
                        self.drop_views();
                    }
                    self.set(f, *dest, w);
                    self.view_of(f, *dest);
                }
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
                let call = self.call_checked(self.print, &[self.ctx, v]);
                let w = self.b.inst_results(call)[0];
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
                let roots = self.epoch_mark();
                let prev = self.push_call();
                self.b.ins().call(self.free_early, &[self.ctx, v, d]);
                self.pop_call(prev);
                self.epoch_check(roots);
                self.drop_views();
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(fast);
                let v = self.get(f, *value);
                let v = self.coerce(v, types::I64);
                let d = self.b.ins().iconst(types::I64, i64::from(*deep));
                self.b.ins().call(self.free_early, &[self.ctx, v, d]);
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(join);
                self.bail_on_helper();
                if numeric(ty_of(f, *dest)) {
                    let zero = self.b.ins().iconst(types::I64, 0);
                    self.set(f, *dest, zero);
                }
            }
            MirInst::Format { dest, fmt, args } => {
                let p = self.words_slot(f, args);
                let fw = self.get(f, *fmt);
                let n = self.b.ins().iconst(types::I64, args.len() as i64);
                let call = self.call_checked(self.format, &[self.ctx, fw, p, n]);
                let w = self.b.inst_results(call)[0];
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
                let call = self.call_checked(self.stringify, &[self.ctx, v, op]);
                let w = self.b.inst_results(call)[0];
                self.set(f, *dest, w);
            }
            MirInst::StrEq { dest, lhs, rhs, ne } => {
                let a = self.get(f, *lhs);
                let a = self.coerce(a, types::I64);
                let c = self.get(f, *rhs);
                let c = self.coerce(c, types::I64);
                let call = self.call_checked(self.eq, &[self.ctx, a, c]);
                let w = self.b.inst_results(call)[0];
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
            // A share of no field marks the object in place: no call, so a
            // body whose only share is behind a `kinds` test stays a leaf
            // (#2506). The header's kind byte says whether the word is a
            // class instance or an array, the objects `Heap::cow_share`
            // marks; anything else (null, a tagged word's object of another
            // kind) is left as it is, as the helper leaves it. A pinned
            // object (archive 8.2) is never marked: the cold path calls the
            // helper, which hands back a copy of it for the new name.
            MirInst::Cow { dest, src, unique: false, field: None } => {
                let s = self.get(f, *src);
                let s = self.coerce(s, types::I64);
                let flags = MemFlagsData::trusted();
                let o = self.b.ins().band_imm_u(s, !1i64);
                let (check, test, mark, copy, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
                let word = self.b.append_block_param(join, types::I64);
                self.b.ins().brif(o, check, &[], join, &[s.into()]);
                self.b.switch_to_block(check);
                let k = self.b.ins().uload8(types::I64, flags, o, machine::HEADER_KIND_OFFSET);
                let inst = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(machine::KIND_INSTANCE));
                let arr = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(machine::KIND_ARRAY));
                let either = self.b.ins().bor(inst, arr);
                if self.pins {
                    self.b.ins().brif(either, test, &[], join, &[s.into()]);
                    self.b.switch_to_block(test);
                    let bits = self.b.ins().uload8(types::I64, flags, o, machine::HEADER_FLAGS_OFFSET);
                    let pinned = self.b.ins().band_imm_u(bits, i64::from(machine::FLAG_PINNED));
                    self.b.ins().brif(pinned, copy, &[], mark, &[]);
                } else {
                    // No body pins: no object is pinned, so no copy path.
                    self.b.ins().brif(either, mark, &[], join, &[s.into()]);
                }
                self.b.switch_to_block(mark);
                let bits = self.b.ins().uload8(types::I32, flags, o, machine::HEADER_FLAGS_OFFSET);
                let set = self.b.ins().bor_imm_u(bits, i64::from(machine::FLAG_SHARED));
                self.b.ins().istore8(flags, set, o, machine::HEADER_FLAGS_OFFSET);
                self.b.ins().jump(join, &[s.into()]);
                if self.pins {
                    self.b.set_cold_block(copy);
                    self.b.switch_to_block(copy);
                    let share = self.b.ins().iconst(types::I64, 0);
                    let call = self.b.ins().call(self.cow, &[self.ctx, s, share]);
                    let w = self.b.inst_results(call)[0];
                    self.bail_on_helper();
                    self.b.ins().jump(join, &[w.into()]);
                }
                self.b.switch_to_block(join);
                self.set_word(f, *dest, word);
                self.view_of(f, *dest);
            }
            // The pin is a header flag beside the shared one, set for a class
            // instance or an array only, as a share marks only those.
            MirInst::CowPin { dest, src } => {
                let s = self.get(f, *src);
                let s = self.coerce(s, types::I64);
                let flags = MemFlagsData::trusted();
                let o = self.b.ins().band_imm_u(s, !1i64);
                let (check, pin, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                let was = self.b.append_block_param(join, types::I64);
                let zero = self.b.ins().iconst(types::I64, 0);
                self.b.ins().brif(o, check, &[], join, &[zero.into()]);
                self.b.switch_to_block(check);
                let k = self.b.ins().uload8(types::I64, flags, o, machine::HEADER_KIND_OFFSET);
                let inst = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(machine::KIND_INSTANCE));
                let arr = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(machine::KIND_ARRAY));
                let either = self.b.ins().bor(inst, arr);
                self.b.ins().brif(either, pin, &[], join, &[zero.into()]);
                self.b.switch_to_block(pin);
                let bits = self.b.ins().uload8(types::I64, flags, o, machine::HEADER_FLAGS_OFFSET);
                let set = self.b.ins().bor_imm_u(bits, i64::from(machine::FLAG_PINNED));
                self.b.ins().istore8(flags, set, o, machine::HEADER_FLAGS_OFFSET);
                let bit = self.b.ins().ushr_imm_u(bits, i64::from(machine::FLAG_PINNED.trailing_zeros()));
                let p = self.b.ins().band_imm_u(bit, 1);
                self.b.ins().jump(join, &[p.into()]);
                self.b.switch_to_block(join);
                self.set(f, *dest, was);
            }
            MirInst::CowUnpin { dest, src, saved } => {
                let s = self.get(f, *src);
                let s = self.coerce(s, types::I64);
                let saved = self.get(f, *saved);
                let saved = self.coerce(saved, types::I64);
                let flags = MemFlagsData::trusted();
                let o = self.b.ins().band_imm_u(s, !1i64);
                let (test, check, unpin, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().brif(saved, join, &[], test, &[]);
                self.b.switch_to_block(test);
                self.b.ins().brif(o, check, &[], join, &[]);
                self.b.switch_to_block(check);
                let k = self.b.ins().uload8(types::I64, flags, o, machine::HEADER_KIND_OFFSET);
                let inst = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(machine::KIND_INSTANCE));
                let arr = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(machine::KIND_ARRAY));
                let either = self.b.ins().bor(inst, arr);
                self.b.ins().brif(either, unpin, &[], join, &[]);
                self.b.switch_to_block(unpin);
                let bits = self.b.ins().uload8(types::I32, flags, o, machine::HEADER_FLAGS_OFFSET);
                let clear = self.b.ins().band_imm_u(bits, i64::from(!machine::FLAG_PINNED));
                self.b.ins().istore8(flags, clear, o, machine::HEADER_FLAGS_OFFSET);
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(join);
                let zero = self.b.ins().iconst(types::I64, 0);
                self.set(f, *dest, zero);
            }
            // A word that is an object or null by its type: the count is
            // read and written inline (reference-count plan step 6).
            MirInst::RcInc { dest, src, guard: None } | MirInst::RcDec { dest, src, guard: None } if counted_inline(ty_of(f, *src)) => {
                let s = self.get(f, *src);
                let s = self.coerce(s, types::I64);
                self.emit_rc(s, matches!(inst, MirInst::RcDec { .. }));
                let zero = self.b.ins().iconst(types::I64, 0);
                self.set(f, *dest, zero);
            }
            // A runtime call for now: the count is kept only in a check run
            // until frames own their words (plan steps 4 and 6).
            MirInst::RcInc { dest, src, guard } | MirInst::RcDec { dest, src, guard } => {
                let s = self.get(f, *src);
                let s = self.coerce(s, types::I64);
                let dec = self.b.ins().iconst(types::I64, i64::from(matches!(inst, MirInst::RcDec { .. })));
                match guard {
                    Some(g) => {
                        let g = self.get(f, *g);
                        let g = self.coerce(g, types::I64);
                        let (call, join) = (self.b.create_block(), self.b.create_block());
                        self.b.ins().brif(g, call, &[], join, &[]);
                        self.b.switch_to_block(call);
                        self.b.ins().call(self.rc, &[self.ctx, s, dec]);
                        self.b.ins().jump(join, &[]);
                        self.b.switch_to_block(join);
                    }
                    None => {
                        let (call, join) = self.maybe_object(s);
                        self.b.switch_to_block(call);
                        self.b.ins().call(self.rc, &[self.ctx, s, dec]);
                        self.b.ins().jump(join, &[]);
                        self.b.switch_to_block(join);
                    }
                }
                let zero = self.b.ins().iconst(types::I64, 0);
                self.set(f, *dest, zero);
            }
            // A write's copy test: only a class instance or an array marked
            // shared may need a copy (`Heap::cow_unique` copies it when its
            // count is above one too), so a word that names anything else, or
            // an object not marked, skips the helper. For a field, the test
            // reads the field's word in place when the object is an instance
            // whose slots native code can address; otherwise the helper.
            MirInst::Cow { dest, src, unique: true, field } => {
                let s = self.get(f, *src);
                let s = self.coerce(s, types::I64);
                let t = MemFlagsData::trusted();
                let (call_b, done) = (self.b.create_block(), self.b.create_block());
                let word = self.b.append_block_param(done, types::I64);
                let tested = match field {
                    Some(i) => {
                        let (fast, slow) = self.field_slot(*src, s, *i, true);
                        self.b.switch_to_block(slow);
                        self.b.ins().jump(call_b, &[]);
                        self.b.switch_to_block(fast);
                        let at = self.b.block_params(fast)[0];
                        self.b.ins().load(types::I64, t, at, 0)
                    }
                    None => s,
                };
                let o = if field.is_some() {
                    // A field copy-on-write tests is a class's or an array's
                    // (`hir::cow::cow_ty`, as an element's for `CowIndex`): its
                    // word is `0` or an object, so no heap range or kind test.
                    let (aligned, object) = (self.b.create_block(), self.b.create_block());
                    let tagged = self.b.ins().band_imm_u(tested, 6);
                    self.b.ins().brif(tagged, done, &[s.into()], aligned, &[]);
                    self.b.switch_to_block(aligned);
                    self.b.ins().brif(tested, object, &[], done, &[s.into()]);
                    self.b.switch_to_block(object);
                    self.b.ins().band_imm_u(tested, !1i64)
                } else {
                    let (object, plain) = self.maybe_object(tested);
                    self.b.switch_to_block(plain);
                    self.b.ins().jump(done, &[s.into()]);
                    self.b.switch_to_block(object);
                    let o = self.b.ins().band_imm_u(tested, !1i64);
                    let k = self.b.ins().uload8(types::I64, t, o, machine::HEADER_KIND_OFFSET);
                    let inst = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(machine::KIND_INSTANCE));
                    let arr = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(machine::KIND_ARRAY));
                    let either = self.b.ins().bor(inst, arr);
                    let flag_b = self.b.create_block();
                    self.b.ins().brif(either, flag_b, &[], done, &[s.into()]);
                    self.b.switch_to_block(flag_b);
                    o
                };
                let bits = self.b.ins().uload8(types::I64, t, o, machine::HEADER_FLAGS_OFFSET);
                let shared = self.b.ins().band_imm_u(bits, i64::from(machine::FLAG_SHARED));
                self.b.ins().brif(shared, call_b, &[], done, &[s.into()]);
                self.b.set_cold_block(call_b);
                self.b.switch_to_block(call_b);
                let call = match field {
                    Some(i) => {
                        let i = self.b.ins().iconst(types::I64, i64::from(*i));
                        self.b.ins().call(self.cow_field, &[self.ctx, s, i])
                    }
                    None => {
                        let u = self.b.ins().iconst(types::I64, 1);
                        self.b.ins().call(self.cow, &[self.ctx, s, u])
                    }
                };
                let w = self.b.inst_results(call)[0];
                self.bail_on_helper();
                self.b.ins().jump(done, &[w.into()]);
                self.b.switch_to_block(done);
                self.set_word(f, *dest, word);
                self.view_of(f, *dest);
            }
            // An element's copy test: the element read as `Index` reads it
            // (its bounds checked there), then the shared flag of an object
            // it names; only a shared one calls the helper, which copies a
            // class instance or an array, stores the copy back and hands it
            // back. Dest is the element after the test.
            MirInst::CowIndex { dest, array, index } => {
                let a = self.get(f, *array);
                let a = self.coerce(a, types::I64);
                let i = self.get(f, *index);
                let (fast, slow, join) = self.in_view(f, *array, i, true);
                self.b.switch_to_block(fast);
                let at = self.element(*array, i);
                let w = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
                self.b.ins().jump(join, &[w.into()]);
                self.b.switch_to_block(slow);
                let call = self.call_checked(self.index, &[self.ctx, a, i]);
                let w = self.b.inst_results(call)[0];
                self.b.ins().jump(join, &[w.into()]);
                self.b.switch_to_block(join);
                let elem = self.b.block_params(join)[0];
                let t = MemFlagsData::trusted();
                let (call_b, done) = (self.b.create_block(), self.b.create_block());
                self.b.append_block_param(done, types::I64);
                // The element's static type is a class or an array, so a word
                // there is `0` or an object: an immediate's tag bits or `0`
                // skip the test, and no heap range check is needed (a word
                // out of the heap names an object the helper leaves alone).
                let (aligned, object) = (self.b.create_block(), self.b.create_block());
                let tagged = self.b.ins().band_imm_u(elem, 6);
                self.b.ins().brif(tagged, done, &[elem.into()], aligned, &[]);
                self.b.switch_to_block(aligned);
                self.b.ins().brif(elem, object, &[], done, &[elem.into()]);
                self.b.switch_to_block(object);
                let o = self.b.ins().band_imm_u(elem, !1i64);
                let bits = self.b.ins().uload8(types::I64, t, o, machine::HEADER_FLAGS_OFFSET);
                let shared = self.b.ins().band_imm_u(bits, i64::from(machine::FLAG_SHARED));
                self.b.ins().brif(shared, call_b, &[], done, &[elem.into()]);
                self.b.set_cold_block(call_b);
                self.b.switch_to_block(call_b);
                let i = self.coerce(i, types::I64);
                let call = self.b.ins().call(self.cow_index, &[self.ctx, a, i]);
                let w = self.b.inst_results(call)[0];
                self.bail_on_helper();
                self.b.ins().jump(done, &[w.into()]);
                self.b.switch_to_block(done);
                let elem = self.b.block_params(done)[0];
                self.set_word(f, *dest, elem);
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
                self.bail_on_helper();
                self.set_word(f, *dest, w);
                self.view_of(f, *dest);
            }
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
    fn call(&mut self, f: &MirFunc, dest: ValueId, dest_hi: Option<ValueId>, dest_more: &[ValueId], target: Label, args: &[ValueId]) -> Result<(), String> {
        let (dest, dest_hi) = (&dest, &dest_hi);
        let (callee, fref, vals) = self.call_words(f, target, args)?;
        let roots = if self.plan.may_gc.contains(&callee) { self.epoch_mark() } else { None };
        let prev = if self.plan.runtime_reach.contains(&callee) { Some(self.push_call()) } else { None };
        let call = self.b.ins().call(fref, &vals);
        let results = self.b.inst_results(call).to_vec();
        if let Some(prev) = prev {
            self.pop_call(prev);
        }
        self.epoch_check(roots);
        self.bail_on_helper();
        if self.plan.moves.contains(&callee) {
            self.drop_views();
        }
        self.set(f, *dest, results[0]);
        if let Some(h) = dest_hi {
            let x = *results.get(1).ok_or("two-word call of a one-word body")?;
            self.set(f, *h, x);
        }
        if !dest_more.is_empty() {
            if results.len() != 2 + dest_more.len() {
                return Err(format!("call of {} words to a body that returns {}", 2 + dest_more.len(), results.len()));
            }
            for (w, &x) in dest_more.iter().zip(&results[2..]) {
                self.set(f, *w, x);
            }
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
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn inline_alloc(&mut self, f: &MirFunc, obj: &machine::native::InlineObject, kind: MirAllocKind, words: &[ValueId], extra: u64, join: Block, reuse: Option<cranelift_codegen::ir::Value>, owned: u32) {
        use machine::native::{
            BUMP_BYTES, BUMP_END, BUMP_FREE, BUMP_FREE_LINK, BUMP_NEXT, COUNTS_ALLOC_BYTES, COUNTS_FIN_LEN, COUNTS_FIN_WORDS, COUNTS_INLINE_GATE,
            COUNTS_HEADER_XOR, COUNTS_LIVE_COUNT, NATIVE_CTX_ALLOC_BUMPS, NATIVE_CTX_ALLOC_COUNTS,
        };
        let mem = MemFlagsData::trusted();
        let slow = self.b.create_block();
        let bump = self.b.create_block();
        let fast = self.b.create_block();
        let counts = self.b.ins().load(types::I64, mem, self.ctx, NATIVE_CTX_ALLOC_COUNTS);
        let gate = self.b.ins().load(types::I64, mem, counts, COUNTS_INLINE_GATE);
        self.b.append_block_param(fast, types::I64);
        // A type with a `drop` stays out of the nursery: the helper
        // allocates it old and lists it. Jumps to `ok` (with `args`) when
        // the inline allocation may go on.
        let young_ok = |this: &mut Self, ok: Block, args: &[cranelift_codegen::ir::BlockArg]| {
            let MirAllocKind::Object { type_id, .. } = kind else {
                this.b.ins().brif(gate, ok, args, slow, &[]);
                return;
            };
            let check = this.b.create_block();
            this.b.ins().brif(gate, check, &[], slow, &[]);
            this.b.switch_to_block(check);
            let word = i64::from(type_id / 64);
            let len = this.b.ins().load(types::I64, mem, counts, COUNTS_FIN_LEN);
            let past = this.b.ins().icmp_imm_s(IntCC::UnsignedLessThanOrEqual, len, word);
            let test = this.b.create_block();
            this.b.ins().brif(past, ok, args, test, &[]);
            this.b.switch_to_block(test);
            let base = this.b.ins().load(types::I64, mem, counts, COUNTS_FIN_WORDS);
            let bits = this.b.ins().load(types::I64, mem, base, (word * 8) as i32);
            let bit = this.b.ins().band_imm_s(bits, 1_i64 << (type_id % 64));
            this.b.ins().brif(bit, slow, &[], ok, args);
        };
        // A slot `RcReuse` kept, a young enum's or instance's (one layout):
        // the image goes there, with no bump and no free list.
        if let Some(slot) = reuse {
            let (kept, take, fresh) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
            self.b.ins().brif(slot, kept, &[], fresh, &[]);
            self.b.switch_to_block(kept);
            young_ok(self, take, &[]);
            // The placeholder the slot holds goes (`Heap::hold_slot`): the
            // new object is counted in below.
            self.b.switch_to_block(take);
            for (off, by) in [(COUNTS_ALLOC_BYTES, machine::Heap::HELD_SLOT_BYTES as i64), (COUNTS_LIVE_COUNT, 1)] {
                let c = self.b.ins().load(types::I64, mem, counts, off);
                let c = self.b.ins().iadd_imm_s(c, -by);
                self.b.ins().store(mem, c, counts, off);
            }
            self.b.ins().jump(fast, &[slot.into()]);
            self.b.switch_to_block(fresh);
        }
        young_ok(self, bump, &[]);
        self.b.switch_to_block(bump);
        let bumps = self.b.ins().load(types::I64, mem, self.ctx, NATIVE_CTX_ALLOC_BUMPS);
        let cursor = self.b.ins().iadd_imm_s(bumps, i64::from(obj.class) * i64::from(BUMP_BYTES));
        // The bump pointer first; when its chunk is spent, a slot freed
        // early since the last minor (#2881), else the helper. Programs that
        // free nothing pay no extra load on the fast path.
        let at = self.b.ins().load(types::I64, mem, cursor, BUMP_NEXT);
        let end = self.b.ins().load(types::I64, mem, cursor, BUMP_END);
        let next = self.b.ins().iadd_imm_s(at, i64::from(obj.slot));
        let fits = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, next, end);
        let bumped = self.b.create_block();
        let spent = self.b.create_block();
        self.b.ins().brif(fits, bumped, &[], spent, &[]);
        self.b.switch_to_block(bumped);
        self.b.ins().store(mem, next, cursor, BUMP_NEXT);
        self.b.ins().jump(fast, &[at.into()]);
        self.b.switch_to_block(spent);
        let free = self.b.ins().load(types::I64, mem, cursor, BUMP_FREE);
        let reuse = self.b.create_block();
        self.b.ins().brif(free, reuse, &[], slow, &[]);
        self.b.switch_to_block(reuse);
        let link = self.b.ins().load(types::I64, mem, free, BUMP_FREE_LINK);
        self.b.ins().store(mem, link, cursor, BUMP_FREE);
        self.b.ins().jump(fast, &[free.into()]);
        self.b.switch_to_block(fast);
        let at = self.b.block_params(fast)[0];
        let first = obj.words_at as usize;
        for (i, &img) in obj.image.iter().enumerate() {
            let x = match i.checked_sub(first).and_then(|k| words.get(k)) {
                Some(&v) => {
                    let x = self.get(f, v);
                    self.coerce(x, types::I64)
                }
                // The header: a steal epoch's mutator makes it old and
                // stamps its epoch (`HeapCounts::header_xor`, zero else).
                None if i == 0 => {
                    let xor = self.b.ins().load(types::I64, mem, counts, COUNTS_HEADER_XOR);
                    self.b.ins().bxor_imm_s(xor, img as i64)
                }
                None => self.b.ins().iconst(types::I64, img as i64),
            };
            self.b.ins().store(mem, x, at, (i * 8) as i32);
        }
        // Each payload word that names an object gains an owner, as the
        // runtime's allocation counts them (an instance's fields are stored
        // after it is made), but for one the frame's owner moves into.
        if matches!(kind, MirAllocKind::Enum { .. }) {
            let kinds = (extra >> 32) as u32;
            for (k, &v) in words.iter().enumerate() {
                let word_kind = if k < 16 { ((kinds >> (2 * k)) & 3) as u8 } else { common::WORD_UNKNOWN };
                let vt = ty_of(f, v);
                if matches!(word_kind, common::WORD_SCALAR | common::WORD_BYTE) || matches!(vt, MirTy::F64 | MirTy::Bool) {
                    continue;
                }
                if k < 32 && owned & (1 << k) != 0 {
                    continue;
                }
                let x = self.get(f, v);
                let x = self.coerce(x, types::I64);
                if counted_inline(vt) {
                    self.emit_rc(x, false);
                } else {
                    let (call, done) = self.maybe_object(x);
                    self.b.switch_to_block(call);
                    let zero = self.b.ins().iconst(types::I64, 0);
                    self.b.ins().call(self.rc, &[self.ctx, x, zero]);
                    self.b.ins().jump(done, &[]);
                    self.b.switch_to_block(done);
                }
            }
        }
        for (off, by) in [(COUNTS_ALLOC_BYTES, i64::from(obj.size)), (COUNTS_LIVE_COUNT, 1)] {
            let c = self.b.ins().load(types::I64, mem, counts, off);
            let c = self.b.ins().iadd_imm_s(c, by);
            self.b.ins().store(mem, c, counts, off);
        }
        self.b.ins().jump(join, &[at.into()]);
        self.b.switch_to_block(slow);
    }

    /// Before a return: use every `gc::root` handle, so each one stays
    /// live until the body returns, as an unused `let r = root(h)` asks.
    fn keep_roots(&mut self, f: &MirFunc) {
        for v in self.root_handles.clone() {
            self.keep_word(f, v);
        }
    }

    /// A use of `v` that does nothing else: its word stays live to here.
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
        self.words_slot_consts(f, vs, last.as_slice())
    }

    /// An extern call on the runtime's libffi call (`coil_native_ffi`),
    /// with no address: its result word.
    fn runtime_extern_call(&mut self, f: &MirFunc, dest: ValueId, sig: &compiler::native_mir::MirExternSig, args: &[ValueId]) -> cranelift_codegen::ir::Value {
        self.runtime_ffi = true;
        let words: Vec<ValueId> = args.iter().take(2).chain(args.iter().skip(3)).copied().collect();
        let tags: Vec<i64> = sig.variadic.iter().flatten().map(|&t| i64::from(t)).collect();
        let operand = (words.len() as u64 - 2) | if sig.variadic.is_some() { 1 << 16 } else { 0 };
        let o = self.b.ins().iconst(types::I64, i64::from(common::FFI_OP_CALL));
        let x = self.b.ins().iconst(types::I64, operand as i64);
        let p = self.words_slot_consts(f, &words, &tags);
        let n = self.b.ins().iconst(types::I64, (words.len() + tags.len()) as i64);
        let roots = self.epoch_mark();
        let prev = self.push_call();
        let call = self.b.ins().call(self.ffi, &[self.ctx, o, x, p, n]);
        let w = self.b.inst_results(call)[0];
        self.pop_call(prev);
        self.epoch_check(roots);
        self.bail_on_helper();
        self.coerce(w, cl_ty(ty_of(f, dest)))
    }

    /// An extern call straight to the C function at `args[2]` (its
    /// address static), on `args[3..]` as `params`: its result word. No
    /// coil code runs and nothing collects during it (the program makes no
    /// callback), so it needs no roots. The runtime makes each pointer
    /// argument (lent in place or a copy, written back after the call) and
    /// packs each struct one; one that cannot be passed panics.
    #[allow(clippy::too_many_arguments)]
    fn direct_extern_call(
        &mut self,
        f: &MirFunc,
        dest: ValueId,
        sig: &compiler::native_mir::MirExternSig,
        args: &[ValueId],
        params: &[CArg],
        ret: CRet,
        va: Option<CVa>,
    ) -> cranelift_codegen::ir::Value {
        use cranelift_codegen::ir::{ArgumentPurpose, StackSlotData, StackSlotKind};
        let words = &args[3..];
        let ptrs: Vec<usize> = (0..params.len()).filter(|&i| matches!(params[i], CArg::Ptr(_))).collect();
        let structs: Vec<usize> = (0..params.len()).filter(|&i| matches!(params[i], CArg::Struct { .. })).collect();
        let want = cl_ty(ty_of(f, dest));
        let mut made = HashMap::new();
        let mut packed = HashMap::new();
        if !ptrs.is_empty() {
            let slot = self.b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, (ptrs.len() * 8) as u32, 3));
            for (k, &i) in ptrs.iter().enumerate() {
                let CArg::Ptr(kind) = params[i] else { continue };
                let w = self.get(f, words[i]);
                let w = self.coerce(w, types::I64);
                let mut kind_v = self.b.ins().iconst(types::I64, kind as i64);
                // A buffer that is another argument too is copied (as the
                // runtime's call does), so C sees no aliasing.
                if kind == machine::native::FFI_ARG_CONST_BYTES || kind == machine::native::FFI_ARG_BYTES {
                    for (j, &other) in words.iter().enumerate() {
                        if j != i {
                            let o = self.get(f, other);
                            let o = self.coerce(o, types::I64);
                            let same = self.b.ins().icmp(IntCC::Equal, w, o);
                            let same = self.b.ins().uextend(types::I64, same);
                            let bit = self.b.ins().ishl_imm_u(same, 8);
                            kind_v = self.b.ins().bor(kind_v, bit);
                        }
                    }
                }
                let out = self.b.ins().stack_addr(types::I64, slot, (k * 8) as i32);
                self.b.ins().call(self.ffi_arg, &[self.ctx, kind_v, w, out]);
                made.insert(i, (slot, k));
            }
        }
        // Each struct argument's C bytes, in a buffer of its own (16-byte
        // aligned, as Windows x64 wants a copy it is lent).
        for &i in &structs {
            let CArg::Struct { id, size, .. } = params[i] else { continue };
            let slot = self.b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, 4));
            let w = self.get(f, words[i]);
            let w = self.coerce(w, types::I64);
            let id_v = self.b.ins().iconst(types::I64, i64::from(id));
            let at = self.b.ins().stack_addr(types::I64, slot, 0);
            self.b.ins().call(self.ffi_struct_in, &[self.ctx, id_v, w, at]);
            packed.insert(i, slot);
        }
        // An argument that cannot be passed has set its panic.
        if !ptrs.is_empty() || !structs.is_empty() {
            self.bail_on_helper();
        }
        let mut csig = Signature::new(self.c_conv);
        let mut cargs = Vec::with_capacity(params.len() + 1);
        // A struct result too large for registers: C writes it through a
        // hidden pointer, the first argument (x8 on AArch64).
        let ret_slot = match ret {
            CRet::Struct { size, .. } => Some(self.b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size.max(32), 4))),
            _ => None,
        };
        if let (CRet::Struct { n: 0, .. }, Some(slot)) = (ret, ret_slot) {
            csig.params.push(AbiParam::special(types::I64, ArgumentPurpose::StructReturn));
            cargs.push(self.b.ins().stack_addr(types::I64, slot, 0));
        }
        let zero_words = |b: &mut FunctionBuilder, csig: &mut Signature, cargs: &mut Vec<_>, ty: Type, n: u8| {
            for _ in 0..n {
                csig.params.push(AbiParam::new(ty));
                cargs.push(if ty == types::F64 { b.ins().f64const(0.0) } else { b.ins().iconst(types::I64, 0) });
            }
        };
        for (i, (&a, p)) in words.iter().zip(params).enumerate() {
            if let (Some(CVa::Stack { gpad, fpad }), true) = (va, i == sig.params.len()) {
                zero_words(&mut self.b, &mut csig, &mut cargs, types::I64, gpad);
                zero_words(&mut self.b, &mut csig, &mut cargs, types::F64, fpad);
            }
            match *p {
                CArg::Callback(aux) => {
                    csig.params.push(AbiParam::new(types::I64));
                    let cb = self.cb.as_ref().expect("a callback argument runs in a program that calls back");
                    let thunk = cb.thunks[&(cb.pcs[&a], aux)];
                    cargs.push(self.b.ins().func_addr(types::I64, thunk));
                }
                CArg::Ptr(_) => {
                    csig.params.push(AbiParam::new(types::I64));
                    let (slot, k) = made[&i];
                    cargs.push(self.b.ins().stack_load(types::I64, types::I64, slot, (k * 8) as i32));
                }
                CArg::Word(t) => {
                    csig.params.push(AbiParam::new(t));
                    let w = self.get(f, a);
                    cargs.push(match t {
                        types::F64 => self.coerce(w, types::F64),
                        types::I64 => self.coerce(w, types::I64),
                        // A `bool` word is `0` or `1`: its low byte.
                        _ => {
                            let w = self.coerce(w, types::I64);
                            self.b.ins().ireduce(t, w)
                        }
                    });
                }
                CArg::Int(t, signed) => {
                    csig.params.push(AbiParam::new(types::I64));
                    let w = self.get(f, a);
                    let w = self.coerce(w, types::I64);
                    let narrow = self.b.ins().ireduce(t, w);
                    cargs.push(if signed { self.b.ins().sextend(types::I64, narrow) } else { self.b.ins().uextend(types::I64, narrow) });
                }
                CArg::Struct { size, how, .. } => {
                    let slot = packed[&i];
                    match how {
                        SPass::Regs(tys, n) => {
                            for (k, &t) in tys.iter().take(n as usize).enumerate() {
                                csig.params.push(AbiParam::new(t));
                                cargs.push(self.b.ins().stack_load(types::I64, t, slot, (k * 8) as i32));
                            }
                        }
                        SPass::Stack => {
                            csig.params.push(AbiParam::special(types::I64, ArgumentPurpose::StructArgument(size)));
                            cargs.push(self.b.ins().stack_addr(types::I64, slot, 0));
                        }
                        SPass::Ref => {
                            csig.params.push(AbiParam::new(types::I64));
                            cargs.push(self.b.ins().stack_addr(types::I64, slot, 0));
                        }
                    }
                }
            }
        }
        match ret {
            CRet::Void => {}
            CRet::Word(t) | CRet::Int(t, _) => csig.returns.push(AbiParam::new(t)),
            CRet::Bool => csig.returns.push(AbiParam::new(types::I8)),
            CRet::Str => csig.returns.push(AbiParam::new(types::I64)),
            CRet::Struct { regs, n, .. } => csig.returns.extend(regs.iter().take(n as usize).map(|&t| AbiParam::new(t))),
        }
        let addr = self.get(f, args[2]);
        let mut addr = self.coerce(addr, types::I64);
        if let Some(CVa::Thunk { pad }) = va {
            zero_words(&mut self.b, &mut csig, &mut cargs, types::I64, pad);
            csig.params.push(AbiParam::new(types::I64));
            cargs.push(addr);
            addr = self.b.ins().func_addr(types::I64, self.va_call);
        }
        if let Some(CVa::ThunkF { pad }) = va {
            zero_words(&mut self.b, &mut csig, &mut cargs, types::F64, pad);
            csig.params.push(AbiParam::new(types::F64));
            cargs.push(self.b.ins().bitcast(types::F64, MemFlagsData::new(), addr));
            addr = self.b.ins().func_addr(types::I64, self.va_call_f);
        }
        let results: Vec<_> = if let Some(cb) = &self.cb {
            // A callback may run coil code: the trampoline records the
            // context the callback runs in, and a panic in it bails here.
            let shape = (csig.params.iter().map(|p| p.value_type).collect(), csig.returns.iter().map(|p| p.value_type).collect());
            let tramp = cb.calls[&shape];
            let mut targs = vec![self.ctx, addr];
            targs.extend(cargs);
            let roots = self.epoch_mark();
            let prev = self.push_call();
            let call = self.b.ins().call(tramp, &targs);
            let results = self.b.inst_results(call).to_vec();
            self.pop_call(prev);
            self.epoch_check(roots);
            self.bail_on_helper();
            results
        } else {
            let csig = self.b.import_signature(csig);
            let call = self.b.ins().call_indirect(csig, addr, &cargs);
            self.b.inst_results(call).to_vec()
        };
        let r = results.first().copied();
        let out = match (ret, r) {
            (CRet::Bool, Some(r)) => {
                let b = self.b.ins().icmp_imm_u(IntCC::NotEqual, r, 0);
                self.b.ins().uextend(types::I64, b)
            }
            (CRet::Str, Some(r)) => {
                let call = self.call_checked(self.ffi_str, &[self.ctx, r]);
                self.b.inst_results(call)[0]
            }
            (CRet::Word(_), Some(r)) => r,
            (CRet::Int(_, true), Some(r)) => self.b.ins().sextend(types::I64, r),
            (CRet::Int(_, false), Some(r)) => self.b.ins().uextend(types::I64, r),
            (CRet::Struct { id, .. }, _) => {
                let slot = ret_slot.expect("a struct result has its buffer");
                for (k, &w) in results.iter().enumerate() {
                    self.b.ins().stack_store(types::I64, w, slot, (k * 8) as i32);
                }
                let id_v = self.b.ins().iconst(types::I64, i64::from(id));
                let at = self.b.ins().stack_addr(types::I64, slot, 0);
                let call = self.call_checked(self.ffi_struct_out, &[self.ctx, id_v, at]);
                self.b.inst_results(call)[0]
            }
            _ => self.b.ins().iconst(types::I64, 0),
        };
        // A returned string may be an argument's copy: it is read first.
        if !ptrs.is_empty() {
            self.b.ins().call(self.ffi_done, &[self.ctx]);
        }
        self.coerce(out, want)
    }

    /// [`Self::words_slot`] with the constants `tail` after the words.
    fn words_slot_consts(&mut self, f: &MirFunc, vs: &[ValueId], tail: &[i64]) -> cranelift_codegen::ir::Value {
        let n = vs.len() + tail.len();
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
        for (i, &c) in tail.iter().enumerate() {
            let x = self.b.ins().iconst(types::I64, c);
            self.b.ins().stack_store(types::I64, x, slot, ((vs.len() + i) * 8) as i32);
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
        let Some(&(ptr, len)) = self.payload_views.get(&scrutinee) else {
            return self.product_load(s, index);
        };
        // The view when it has the word; otherwise the in-place read,
        // which takes the view for the next read.
        let (have, take, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.append_block_param(done, types::I64);
        let n = self.b.use_var(len);
        let p = self.b.use_var(ptr);
        let has = self.b.ins().icmp_imm_u(IntCC::UnsignedGreaterThan, n, i64::from(index));
        self.b.ins().brif(has, have, &[], take, &[]);
        self.b.switch_to_block(have);
        let w = self.b.ins().load(types::I64, MemFlagsData::trusted(), p, i64::from(index) as i32 * 8);
        self.b.ins().jump(done, &[w.into()]);
        self.b.switch_to_block(take);
        let (fast, slow, view) = self.product_slot_view(s, index, false);
        self.b.switch_to_block(slow);
        let args = self.bail_args(TRAP_HELPER);
        self.b.ins().jump(self.bail, &args);
        self.b.switch_to_block(fast);
        let (vp, vn) = view;
        self.b.def_var(ptr, vp);
        self.b.def_var(len, vn);
        let at = self.b.block_params(fast)[0];
        let w = self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
        self.b.ins().jump(done, &[w.into()]);
        self.b.switch_to_block(done);
        self.b.block_params(done)[0]
    }

    /// Enum object `o`'s `tag | payload length << 32`, read in place, or
    /// from `coil_native_enum_tag` for anything the inline read does not
    /// know (`0`, another kind, another payload storage).
    /// With `view`, it takes the payload's `(pointer, length)` too on the
    /// in-place paths ([`Self::payload_views`]).
    fn enum_tag_word(&mut self, o: cranelift_codegen::ir::Value, view: Option<(Variable, Variable)>) -> cranelift_codegen::ir::Value {
        let o = self.coerce(o, types::I64);
        let l = machine::native::product_slots();
        let flags = MemFlagsData::trusted();
        let (kind, storage, inline, spill_test, spill, slow, done) = (
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
        );
        self.b.append_block_param(done, types::I64);
        self.b.ins().brif(o, kind, &[], slow, &[]);
        self.b.switch_to_block(kind);
        let k = self.b.ins().uload8(types::I64, flags, o, l.kind_at as i32);
        let is_enum = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(l.enum_kind));
        self.b.ins().brif(is_enum, storage, &[], slow, &[]);
        self.b.switch_to_block(storage);
        let tag = self.b.ins().uload32(flags, o, l.enum_tag_at as i32);
        let st = self.b.ins().uload8(types::I64, flags, o, l.storage_at as i32);
        let is_inline = self.b.ins().icmp_imm_s(IntCC::Equal, st, i64::from(l.inline_tag));
        self.b.ins().brif(is_inline, inline, &[], spill_test, &[]);
        self.b.switch_to_block(inline);
        let len = self.b.ins().uload8(types::I64, flags, o, l.inline_len_at as i32);
        if let Some((vp, vn)) = view {
            let ptr = self.b.ins().iadd_imm_s(o, i64::from(l.inline_slots_at));
            self.b.def_var(vp, ptr);
            self.b.def_var(vn, len);
        }
        let hi = self.b.ins().ishl_imm_u(len, 32);
        let tl = self.b.ins().bor(tag, hi);
        self.b.ins().jump(done, &[tl.into()]);
        self.b.switch_to_block(spill_test);
        let is_spill = self.b.ins().icmp_imm_s(IntCC::Equal, st, i64::from(l.spill_tag));
        self.b.ins().brif(is_spill, spill, &[], slow, &[]);
        self.b.switch_to_block(spill);
        let len = self.b.ins().load(types::I64, flags, o, l.spill_len_at as i32);
        if let Some((vp, vn)) = view {
            let ptr = self.b.ins().load(types::I64, flags, o, l.spill_ptr_at as i32);
            self.b.def_var(vp, ptr);
            self.b.def_var(vn, len);
        }
        let hi = self.b.ins().ishl_imm_u(len, 32);
        let tl = self.b.ins().bor(tag, hi);
        self.b.ins().jump(done, &[tl.into()]);
        self.b.switch_to_block(slow);
        let call = self.b.ins().call(self.enum_tag, &[o]);
        let tl = self.b.inst_results(call)[0];
        self.b.ins().jump(done, &[tl.into()]);
        self.b.switch_to_block(done);
        self.b.block_params(done)[0]
    }

    /// Word `slot` of product object `o`, read in place
    /// ([`Self::product_slot`]). Only a compiler bug reads a slot of
    /// anything else, or past the object: that bails (a panic).
    fn product_load(&mut self, o: cranelift_codegen::ir::Value, slot: u32) -> cranelift_codegen::ir::Value {
        let o = self.coerce(o, types::I64);
        let (fast, slow) = self.product_slot(o, slot, false);
        self.b.switch_to_block(slow);
        let args = self.bail_args(TRAP_HELPER);
        self.b.ins().jump(self.bail, &args);
        self.b.switch_to_block(fast);
        let at = self.b.block_params(fast)[0];
        self.b.ins().load(types::I64, MemFlagsData::trusted(), at, 0)
    }

    /// Fill `v`'s view right after `v` is defined, when the body indexes it.
    fn view_of(&mut self, f: &MirFunc, v: ValueId) {
        self.lane_view_of(f, v);
        self.fill_view(f, v);
    }

    /// Take `array`'s word view again when it was dropped (an op that may
    /// move its storage ran since it was taken), in a cold block, so the
    /// element accesses after a call or a push stay in place.
    fn ensure_view(&mut self, f: &MirFunc, array: ValueId) {
        let (ok, _, _) = self.views[&array];
        let state = self.b.use_var(ok);
        let unknown = self.b.ins().icmp_imm_s(IntCC::Equal, state, VIEW_UNKNOWN);
        let (fill, have) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(unknown, fill, &[], have, &[]);
        self.b.set_cold_block(fill);
        self.b.switch_to_block(fill);
        self.fill_view(f, array);
        self.b.ins().jump(have, &[]);
        self.b.switch_to_block(have);
    }

    /// `v`'s word view, from the runtime ([`machine::native::coil_native_view`]).
    fn fill_view(&mut self, f: &MirFunc, v: ValueId) {
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

    /// Fill `v`'s lane view right after `v` is defined, when the body
    /// reads or writes its elements in place ([`Self::view_of`]).
    fn lane_view_of(&mut self, f: &MirFunc, v: ValueId) {
        let Some(&(ok, ptr, len)) = self.lane_views.get(&v) else {
            return;
        };
        let a = self.get(f, v);
        let a = self.coerce(a, types::I64);
        let store = self.b.ins().iconst(types::I64, i64::from(self.stored.contains(&v)));
        let k = self.b.ins().iconst(types::I64, i64::from(self.lane_words.get(&v).copied().unwrap_or(2)));
        let out = self.b.ins().stack_addr(types::I64, self.view_slot, 0);
        let call = self.b.ins().call(self.lane_view, &[a, store, k, out]);
        let r = self.b.inst_results(call)[0];
        let p = self.b.ins().stack_load(types::I64, types::I64, self.view_slot, 0);
        let n = self.b.ins().stack_load(types::I64, types::I64, self.view_slot, 8);
        // 1: in lanes; 2: one word per element.
        let two = self.b.ins().iconst(types::I64, 2);
        let state = self.b.ins().isub(two, r);
        self.b.def_var(ok, state);
        self.b.def_var(ptr, p);
        self.b.def_var(len, n);
    }

    /// Branch on whether element `i` of lane array `array` is in its lane
    /// view: `(in view, otherwise)`; with no lane view, always otherwise.
    /// A view not known (dropped after a call or a push) is filled first.
    fn in_lane_view(&mut self, f: &MirFunc, array: ValueId, i: cranelift_codegen::ir::Value) -> (Block, Block) {
        let fast = self.b.create_block();
        let slow = self.b.create_block();
        match self.lane_views.get(&array).copied() {
            Some((ok, _, len)) => {
                let (fill, have) = (self.b.create_block(), self.b.create_block());
                let state = self.b.use_var(ok);
                self.b.ins().brif(state, have, &[], fill, &[]);
                self.b.set_cold_block(fill);
                self.b.switch_to_block(fill);
                self.lane_view_of(f, array);
                self.b.ins().jump(have, &[]);
                self.b.switch_to_block(have);
                let ok = self.b.use_var(ok);
                let len = self.b.use_var(len);
                let inb = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
                let has = self.b.ins().icmp_imm_s(IntCC::Equal, ok, 1);
                let c = self.b.ins().band(inb, has);
                self.b.ins().brif(c, fast, &[], slow, &[]);
            }
            None => {
                self.b.ins().jump(slow, &[]);
            }
        }
        (fast, slow)
    }

    /// Address of element `i`'s first word in `array`'s lane view (two
    /// words per element, or a wide element's three or four).
    fn lane_element(&mut self, array: ValueId, i: cranelift_codegen::ir::Value) -> cranelift_codegen::ir::Value {
        let (_, ptr, _) = self.lane_views[&array];
        let p = self.b.use_var(ptr);
        let off = match self.lane_words.get(&array).copied().unwrap_or(2) {
            2 => self.b.ins().ishl_imm_u(i, 4),
            4 => self.b.ins().ishl_imm_u(i, 5),
            k => self.b.ins().imul_imm_s(i, 8 * i64::from(k)),
        };
        self.b.ins().iadd(p, off)
    }

    /// `words` written to the stack buffer for a wide lane helper; its
    /// address.
    fn lane_buffer(&mut self, words: &[cranelift_codegen::ir::Value]) -> cranelift_codegen::ir::Value {
        for (n, &w) in words.iter().enumerate() {
            self.b.ins().stack_store(types::I64, w, self.view_slot, 8 * n as i32);
        }
        self.b.ins().stack_addr(types::I64, self.view_slot, 0)
    }

    /// `(pointer, length)` of string `s`'s bytes from its view, filled
    /// first when it is not known (length 0: not a string object itself).
    fn str_bytes(&mut self, f: &MirFunc, s: ValueId) -> (cranelift_codegen::ir::Value, cranelift_codegen::ir::Value) {
        let (known, ptr, len) = self.str_views[&s];
        let (fetch, have) = (self.b.create_block(), self.b.create_block());
        let k = self.b.use_var(known);
        self.b.ins().brif(k, have, &[], fetch, &[]);
        self.b.set_cold_block(fetch);
        self.b.switch_to_block(fetch);
        let sw = self.get(f, s);
        let sw = self.coerce(sw, types::I64);
        let out = self.b.ins().stack_addr(types::I64, self.view_slot, 0);
        let call = self.b.ins().call(self.str_view, &[sw, out]);
        let r = self.b.inst_results(call)[0];
        let p = self.b.ins().stack_load(types::I64, types::I64, self.view_slot, 0);
        let n = self.b.ins().stack_load(types::I64, types::I64, self.view_slot, 8);
        self.b.def_var(known, r);
        self.b.def_var(ptr, p);
        self.b.def_var(len, n);
        self.b.ins().jump(have, &[]);
        self.b.switch_to_block(have);
        (self.b.use_var(ptr), self.b.use_var(len))
    }

    /// A forward `ByteScan` over strings and byte arrays: test 16 lanes at a
    /// time in place for up to one kernel's width
    /// ([`common::simd::BYTE_LANES`]), so a scan whose next hit is near ends
    /// without entering the lane kernel. Past that width, or where a scalar
    /// is no byte or a stream is neither, the kernel goes on from the lane
    /// reached. Where 16 lanes would leave the bound or a stream, the scan
    /// stops at the lane reached: fewer than 16 iterations are left to the
    /// loop, cheaper than a kernel call. Stopping at a lane the kernel would
    /// pass is never wrong: the loop after the scan runs it. `None`: not
    /// such a scan (nothing emitted).
    fn scan_in_place(&mut self, f: &MirFunc, inst: &MirInst) -> Option<cranelift_codegen::ir::Value> {
        const WIDE: i64 = 16;
        let MirInst::ByteScan { from, bound, inclusive, backward: false, streams, scalars, expr, .. } = inst else {
            return None;
        };
        if streams.is_empty() || !streams.iter().all(|(s, _)| self.str_views.contains_key(s)) || !lane_expr_fits(expr, streams.len(), scalars.len()) {
            return None;
        }
        let mut bytes = Vec::with_capacity(streams.len());
        let mut kinds = Vec::with_capacity(streams.len());
        for &(st, off) in streams {
            let (p, n, known) = self.stream_bytes(f, st);
            let o = self.get(f, off);
            let o = self.coerce(o, types::I64);
            bytes.push((p, n, o));
            kinds.push(known);
        }
        let scal: Vec<_> = scalars
            .iter()
            .map(|&v| {
                let x = self.get(f, v);
                self.coerce(x, types::I64)
            })
            .collect();
        let from = self.get(f, *from);
        let from = self.coerce(from, types::I64);
        let bound = self.get(f, *bound);
        let bound = self.coerce(bound, types::I64);
        let (head, more, test, found, step, kern, done) = (
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
        );
        for blk in [head, found] {
            self.b.append_block_param(blk, types::I64);
        }
        self.b.append_block_param(head, types::I64);
        // The lane mask `vhigh_bits` gives.
        self.b.append_block_param(found, types::I32);
        self.b.append_block_param(step, types::I64);
        self.b.append_block_param(kern, types::I64);
        self.b.append_block_param(done, types::I64);
        // Lane compares are on bytes: a scalar outside `0..=255` leaves it to the kernel.
        // So does a stream that is neither a string nor a byte array.
        let mut bytes_ok = self.b.ins().iconst(types::I8, 1);
        for &x in &scal {
            let ok = self.b.ins().icmp_imm_u(IntCC::UnsignedLessThanOrEqual, x, 255);
            bytes_ok = self.b.ins().band(bytes_ok, ok);
        }
        for &k in &kinds {
            let ok = self.b.ins().icmp_imm_s(IntCC::NotEqual, k, 0);
            bytes_ok = self.b.ins().band(bytes_ok, ok);
        }
        let budget = self.b.ins().iconst(types::I64, common::simd::BYTE_LANES as i64 / WIDE);
        self.b.ins().brif(bytes_ok, head, &[from.into(), budget.into()], kern, &[from.into()]);
        // head(p, left): out of budget, or 16 lanes past the bound, the kernel goes on.
        self.b.switch_to_block(head);
        let (p, left) = (self.b.block_params(head)[0], self.b.block_params(head)[1]);
        self.b.ins().brif(left, more, &[], kern, &[p.into()]);
        self.b.switch_to_block(more);
        // The last of the 16 lanes against the bound; past `MAX` it is
        // past any bound (#2522).
        let wide = self.b.ins().iconst(types::I64, WIDE - 1);
        let (last, past) = self.b.ins().sadd_overflow(p, wide);
        let cc = if *inclusive { IntCC::SignedLessThanOrEqual } else { IntCC::SignedLessThan };
        let below = self.b.ins().icmp(cc, last, bound);
        let fits = self.b.ins().bxor_imm_u(past, 1);
        let inb = self.b.ins().band(below, fits);
        self.b.ins().brif(inb, test, &[], done, &[p.into()]);
        self.b.switch_to_block(test);
        let mut lanes = Vec::with_capacity(bytes.len());
        for &(ptr, n, o) in &bytes {
            let at = self.b.ins().iadd(p, o);
            let end = self.b.ins().iadd_imm_s(at, WIDE);
            let lo = self.b.ins().icmp(IntCC::UnsignedLessThan, at, n);
            let hi = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, end, n);
            let ok = self.b.ins().band(lo, hi);
            let read = self.b.create_block();
            self.b.ins().brif(ok, read, &[], done, &[p.into()]);
            self.b.switch_to_block(read);
            let a = self.b.ins().iadd(ptr, at);
            lanes.push(self.b.ins().load(types::I8X16, MemFlagsData::new().with_notrap(), a, 0));
        }
        let mask = self.lane_test(expr, &lanes, &scal);
        let bits = self.b.ins().vhigh_bits(types::I32, mask);
        self.b.ins().brif(bits, found, &[p.into(), bits.into()], step, &[p.into()]);
        self.b.switch_to_block(found);
        let (p, bits) = (self.b.block_params(found)[0], self.b.block_params(found)[1]);
        let k = self.b.ins().ctz(bits);
        let k = self.b.ins().uextend(types::I64, k);
        let at = self.b.ins().iadd(p, k);
        self.b.ins().jump(done, &[at.into()]);
        self.b.switch_to_block(step);
        let p = self.b.block_params(step)[0];
        let next = self.b.ins().iadd_imm_s(p, WIDE);
        let left = self.b.ins().iadd_imm_s(left, -1);
        self.b.ins().jump(head, &[next.into(), left.into()]);
        self.b.set_cold_block(kern);
        self.b.switch_to_block(kern);
        let at = self.b.block_params(kern)[0];
        let (w, _) = self.vector_lanes_at(f, inst, Some(at));
        self.b.ins().jump(done, &[w.into()]);
        self.b.switch_to_block(done);
        Some(self.b.block_params(done)[0])
    }

    /// `(pointer, length)` of the bytes of scan stream `s`: a string's from
    /// its view, else a byte array's packed storage, read at this scan (a
    /// byte array may change between scans), and whether it is either (the
    /// lane kernel reads anything else).
    fn stream_bytes(&mut self, f: &MirFunc, s: ValueId) -> (cranelift_codegen::ir::Value, cranelift_codegen::ir::Value, cranelift_codegen::ir::Value) {
        let (p, n) = self.str_bytes(f, s);
        let l = machine::native::array_slots();
        let t = MemFlagsData::trusted();
        let (arr, kind_b, tag_b, bytes_b, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
        for _ in 0..3 {
            self.b.append_block_param(join, types::I64);
        }
        let (zero, one) = (self.b.ins().iconst(types::I64, 0), self.b.ins().iconst(types::I64, 1));
        let neither = [zero.into(), zero.into(), zero.into()];
        // A string with bytes. An empty one, or not a string, takes the array test.
        self.b.ins().brif(n, join, &[p.into(), n.into(), one.into()], arr, &[]);
        self.b.switch_to_block(arr);
        let a = self.get(f, s);
        let a = self.coerce(a, types::I64);
        self.b.ins().brif(a, kind_b, &[], join, &neither);
        self.b.switch_to_block(kind_b);
        let k = self.b.ins().uload8(types::I64, t, a, l.kind_at as i32);
        let is = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(l.kind));
        self.b.ins().brif(is, tag_b, &[], join, &neither);
        self.b.switch_to_block(tag_b);
        let tag = self.b.ins().uload8(types::I64, t, a, l.data_at as i32);
        let is = self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(l.bytes_tag));
        self.b.ins().brif(is, bytes_b, &[], join, &neither);
        self.b.switch_to_block(bytes_b);
        let bp = self.b.ins().load(types::I64, t, a, l.bytes_ptr_at as i32);
        let bn = self.b.ins().load(types::I64, t, a, l.bytes_len_at as i32);
        self.b.ins().jump(join, &[bp.into(), bn.into(), one.into()]);
        self.b.switch_to_block(join);
        let ps = self.b.block_params(join);
        (ps[0], ps[1], ps[2])
    }

    /// A scan predicate on 16 lanes of bytes (`lanes[k]`, stream `k`) and
    /// the scan's scalars (each in `0..=255`): a lane mask, all ones where
    /// the lane does something.
    fn lane_test(&mut self, e: &compiler::native_mir::LaneExpr, lanes: &[cranelift_codegen::ir::Value], scal: &[cranelift_codegen::ir::Value]) -> cranelift_codegen::ir::Value {
        use compiler::native_mir::{LaneExpr, LaneVal};
        match e {
            LaneExpr::Const(b) => {
                let x = self.b.ins().iconst(types::I8, if *b { -1 } else { 0 });
                self.b.ins().splat(types::I8X16, x)
            }
            LaneExpr::Cmp { op, lhs: LaneVal::Scalar(a), rhs: LaneVal::Scalar(c) } => {
                // The same in every lane.
                let cc = match op {
                    MirCmpOp::Lt => IntCC::SignedLessThan,
                    MirCmpOp::Le => IntCC::SignedLessThanOrEqual,
                    MirCmpOp::Gt => IntCC::SignedGreaterThan,
                    MirCmpOp::Ge => IntCC::SignedGreaterThanOrEqual,
                    MirCmpOp::Eq => IntCC::Equal,
                    MirCmpOp::Ne => IntCC::NotEqual,
                };
                let t = self.b.ins().icmp(cc, scal[usize::from(*a)], scal[usize::from(*c)]);
                let m = self.b.ins().ineg(t);
                self.b.ins().splat(types::I8X16, m)
            }
            LaneExpr::Cmp { op, lhs, rhs } => {
                // Bytes and byte scalars are `0..=255`: their `i64` order is the unsigned byte order.
                let mut val = |v: &LaneVal| match *v {
                    LaneVal::Stream(k) => lanes[usize::from(k)],
                    LaneVal::Scalar(k) => {
                        let x = self.b.ins().ireduce(types::I8, scal[usize::from(k)]);
                        self.b.ins().splat(types::I8X16, x)
                    }
                };
                let (a, c) = (val(lhs), val(rhs));
                let cc = match op {
                    MirCmpOp::Lt => IntCC::UnsignedLessThan,
                    MirCmpOp::Le => IntCC::UnsignedLessThanOrEqual,
                    MirCmpOp::Gt => IntCC::UnsignedGreaterThan,
                    MirCmpOp::Ge => IntCC::UnsignedGreaterThanOrEqual,
                    MirCmpOp::Eq => IntCC::Equal,
                    MirCmpOp::Ne => IntCC::NotEqual,
                };
                self.b.ins().icmp(cc, a, c)
            }
            LaneExpr::And(a, c) => {
                let (a, c) = (self.lane_test(a, lanes, scal), self.lane_test(c, lanes, scal));
                self.b.ins().band(a, c)
            }
            LaneExpr::Or(a, c) => {
                let (a, c) = (self.lane_test(a, lanes, scal), self.lane_test(c, lanes, scal));
                self.b.ins().bor(a, c)
            }
            LaneExpr::Not(a) => {
                let a = self.lane_test(a, lanes, scal);
                self.b.ins().bnot(a)
            }
        }
    }

    /// A host call through the runtime's dispatcher (`coil_native_host`):
    /// its result word. The second word of a pair result is left in the
    /// context for the caller to read.
    fn host_call(&mut self, f: &MirFunc, native_id: u16, layout: u8, args: &[ValueId]) -> cranelift_codegen::ir::Value {
        let fp = self.b.ins().iconst(types::I64, i64::from(native_id));
        let lay = self.b.ins().iconst(types::I64, i64::from(layout));
        let p = self.words_slot(f, args);
        let n = self.b.ins().iconst(types::I64, args.len() as i64);
        // `gc_collect` collects, and a host call through the runtime may:
        // this frame's words are roots.
        let collect = machine::native::direct_host_natives().get(usize::from(native_id)).is_some_and(|h| h.collect || h.on_machine);
        let roots = if collect { self.epoch_mark() } else { None };
        let prev = if collect { Some(self.push_call()) } else { None };
        let call = self.b.ins().call(self.host, &[self.ctx, fp, lay, p, n]);
        let w = self.b.inst_results(call)[0];
        if let Some(prev) = prev {
            self.pop_call(prev);
        }
        self.epoch_check(roots);
        self.bail_on_helper();
        w
    }

    /// The word storage of array `a`: `(pointer, length, capacity)` in the
    /// current block, which is reached only when `a` is a non-null array
    /// holding words; anything else branches to `slow`.
    fn array_words(&mut self, a: cranelift_codegen::ir::Value, slow: Block) -> [cranelift_codegen::ir::Value; 3] {
        let l = machine::native::array_slots();
        let t = MemFlagsData::trusted();
        let (kind_b, tag_b, words_b) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.ins().brif(a, kind_b, &[], slow, &[]);
        self.b.switch_to_block(kind_b);
        let k = self.b.ins().uload8(types::I64, t, a, l.kind_at as i32);
        let is = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(l.kind));
        self.b.ins().brif(is, tag_b, &[], slow, &[]);
        self.b.switch_to_block(tag_b);
        let tag = self.b.ins().uload8(types::I64, t, a, l.data_at as i32);
        let is = self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(l.words_tag));
        self.b.ins().brif(is, words_b, &[], slow, &[]);
        self.b.switch_to_block(words_b);
        let ptr = self.b.ins().load(types::I64, t, a, l.ptr_at as i32);
        let len = self.b.ins().load(types::I64, t, a, l.len_at as i32);
        let cap = self.b.ins().load(types::I64, t, a, l.cap_at as i32);
        [ptr, len, cap]
    }

    /// [`Self::array_words`] of a word its type says is an array (a `Vec`
    /// field of a map object): only the storage's kind is tested. One a
    /// field shares with another field is marked shared, which the caller
    /// tests next.
    fn vec_words(&mut self, a: cranelift_codegen::ir::Value, slow: Block) -> [cranelift_codegen::ir::Value; 3] {
        let l = machine::native::array_slots();
        let t = MemFlagsData::trusted();
        let words_b = self.b.create_block();
        let tag = self.b.ins().uload8(types::I64, t, a, l.data_at as i32);
        let is = self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(l.words_tag));
        self.b.ins().brif(is, words_b, &[], slow, &[]);
        self.b.switch_to_block(words_b);
        let ptr = self.b.ins().load(types::I64, t, a, l.ptr_at as i32);
        let len = self.b.ins().load(types::I64, t, a, l.len_at as i32);
        let cap = self.b.ins().load(types::I64, t, a, l.cap_at as i32);
        [ptr, len, cap]
    }

    /// A well-formed map table `ix` (`machine::map_table::table_cap`):
    /// `(pointer, length, capacity, mask)` of its words in the current
    /// block; a malformed one, or anything but a word array, goes to `slow`.
    fn map_table_words(&mut self, ix: cranelift_codegen::ir::Value, slow: Block) -> [cranelift_codegen::ir::Value; 4] {
        let t = MemFlagsData::trusted();
        let [ptr, len, cap] = self.array_words(ix, slow);
        let (has_mask, ok) = (self.b.create_block(), self.b.create_block());
        let hdr = self.b.ins().icmp_imm_u(IntCC::UnsignedGreaterThanOrEqual, len, machine::map_table::HEADER as i64);
        self.b.ins().brif(hdr, has_mask, &[], slow, &[]);
        self.b.switch_to_block(has_mask);
        let mask = self.b.ins().load(types::I64, t, ptr, 0);
        // `mask + 1` a power of two (and small enough that the cells'
        // size cannot wrap), the words long enough for the cells.
        let small = self.b.ins().icmp_imm_u(IntCC::UnsignedLessThan, mask, 1 << 40);
        let cells = self.b.ins().iadd_imm_s(mask, 1);
        let pow = self.b.ins().band(mask, cells);
        let pow = self.b.ins().icmp_imm_s(IntCC::Equal, pow, 0);
        let need = self.b.ins().ishl_imm_u(cells, 1);
        let need = self.b.ins().iadd_imm_s(need, machine::map_table::HEADER as i64);
        let fits = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, len, need);
        let good = self.b.ins().band(small, pow);
        let good = self.b.ins().band(good, fits);
        self.b.ins().brif(good, ok, &[], slow, &[]);
        self.b.switch_to_block(ok);
        [ptr, len, cap, mask]
    }

    /// `map_table::home(h, mask)`: the probe start.
    fn map_home(&mut self, h: cranelift_codegen::ir::Value, mask: cranelift_codegen::ir::Value) -> cranelift_codegen::ir::Value {
        let s32 = self.b.ins().iconst(types::I64, 32);
        let hi = self.b.ins().sshr(h, s32);
        let h = self.b.ins().bxor(h, hi);
        let s16 = self.b.ins().iconst(types::I64, 16);
        let hi = self.b.ins().sshr(h, s16);
        let h = self.b.ins().bxor(h, hi);
        self.b.ins().band(h, mask)
    }

    /// A map table op, a string hash, in place, with the host call as its
    /// slow path (`docs/internals/native.md`, "Map table ops"). Each reads
    /// what the native reads and gives the same result; `map_upsert` also
    /// writes what the native writes when no growth is due and the table's
    /// storage has room, else leaves everything to the native.
    fn lower_map_op(&mut self, f: &MirFunc, op: MapOp, dest: ValueId, native_id: u16, layout: u8, args: &[ValueId]) {
        let t = MemFlagsData::trusted();
        let (slow, join) = (self.b.create_block(), self.b.create_block());
        self.b.append_block_param(join, types::I64);
        let arg = |this: &mut Self, i: usize| {
            let v = this.get(f, args[i]);
            this.coerce(v, types::I64)
        };
        let hdr = machine::map_table::HEADER as i32 * 8;
        match op {
            MapOp::StrHash => {
                let sl = machine::native::string_slots();
                let s = arg(self, 0);
                let (kind_b, str_b, have) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().brif(s, kind_b, &[], slow, &[]);
                self.b.switch_to_block(kind_b);
                let k = self.b.ins().uload8(types::I64, t, s, sl.kind_at as i32);
                let is = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(sl.kind));
                self.b.ins().brif(is, str_b, &[], slow, &[]);
                self.b.switch_to_block(str_b);
                let w = self.b.ins().load(types::I64, t, s, sl.hash_at as i32);
                let set = self.b.ins().band_imm_u(w, sl.hash_set as i64);
                let compute = self.b.create_block();
                self.b.ins().brif(set, have, &[], compute, &[]);
                self.b.switch_to_block(have);
                let h = self.b.ins().band_imm_u(w, 0xFFFF_FFFF);
                self.b.ins().jump(join, &[h.into()]);
                // Not cached yet (a string just built or sliced): FNV-1a
                // over its bytes here, as `ObjString::hash`, then cache it
                // as `ObjString::hash_code` does (`HASH_SET | h`; the
                // interned bit is only ever set after `HASH_SET`). The
                // bytes come from the leaf view helper, no host call.
                self.b.switch_to_block(compute);
                let out = self.b.ins().stack_addr(types::I64, self.view_slot, 0);
                self.b.ins().call(self.str_view, &[s, out]);
                let p = self.b.ins().stack_load(types::I64, types::I64, self.view_slot, 0);
                let n = self.b.ins().stack_load(types::I64, types::I64, self.view_slot, 8);
                let (head, body, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.append_block_param(head, types::I64);
                self.b.append_block_param(head, types::I32);
                self.b.append_block_param(done, types::I32);
                let zero = self.b.ins().iconst(types::I64, 0);
                let basis = self.b.ins().iconst(types::I32, i64::from(machine::native::FNV_BASIS as i32));
                self.b.ins().jump(head, &[zero.into(), basis.into()]);
                self.b.switch_to_block(head);
                let (i, acc) = (self.b.block_params(head)[0], self.b.block_params(head)[1]);
                let more = self.b.ins().icmp(IntCC::UnsignedLessThan, i, n);
                self.b.ins().brif(more, body, &[], done, &[acc.into()]);
                self.b.switch_to_block(body);
                let at = self.b.ins().iadd(p, i);
                let byte = self.b.ins().uload8(types::I32, t, at, 0);
                let x = self.b.ins().bxor(acc, byte);
                let x = self.b.ins().imul_imm_s(x, i64::from(machine::native::FNV_PRIME as i32));
                let i1 = self.b.ins().iadd_imm_s(i, 1);
                self.b.ins().jump(head, &[i1.into(), x.into()]);
                self.b.switch_to_block(done);
                let h32 = self.b.block_params(done)[0];
                let h = self.b.ins().uextend(types::I64, h32);
                let cached = self.b.ins().bor_imm_u(h, sl.hash_set as i64);
                self.b.ins().store(t, cached, s, sl.hash_at as i32);
                self.b.ins().jump(join, &[h.into()]);
            }
            MapOp::Len => {
                let ix = arg(self, 0);
                let [p, ..] = self.map_table_words(ix, slow);
                let n = self.b.ins().load(types::I64, t, p, 16);
                self.b.ins().jump(join, &[n.into()]);
            }
            MapOp::Live => {
                let ix = arg(self, 0);
                let s = arg(self, 1);
                let [p, len, _, mask] = self.map_table_words(ix, slow);
                // Slot `s`'s word in the tail: its cell, live while in `0..cap`.
                let cells = self.b.ins().iadd_imm_s(mask, 1);
                let tail = self.b.ins().ishl_imm_u(cells, 1);
                let tail = self.b.ins().iadd_imm_s(tail, machine::map_table::HEADER as i64);
                let at = self.b.ins().iadd(tail, s);
                let inb = self.b.ins().icmp(IntCC::UnsignedLessThan, at, len);
                let nonneg = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, s, 0);
                let inb = self.b.ins().band(inb, nonneg);
                let (read, no) = (self.b.create_block(), self.b.create_block());
                self.b.ins().brif(inb, read, &[], no, &[]);
                self.b.switch_to_block(no);
                let zero = self.b.ins().iconst(types::I64, 0);
                self.b.ins().jump(join, &[zero.into()]);
                self.b.switch_to_block(read);
                let off = self.b.ins().ishl_imm_u(at, 3);
                let addr = self.b.ins().iadd(p, off);
                let c = self.b.ins().load(types::I64, t, addr, 0);
                let live = self.b.ins().icmp(IntCC::UnsignedLessThan, c, cells);
                let live = self.b.ins().uextend(types::I64, live);
                self.b.ins().jump(join, &[live.into()]);
            }
            MapOp::Find { string } | MapOp::Upsert { string } | MapOp::Put { string, .. } => {
                let upsert = !matches!(op, MapOp::Find { .. });
                let l = machine::native::array_slots();
                // `map_put`: the map object's three fields, each a word
                // array no other place shares (as `Cow`'s test reads the
                // shared mark: a field holding another field's array is
                // marked so too); else the native, which makes them unique.
                let (ix, keys, h, k, put) = if let MapOp::Put { object, replace, .. } = op {
                    let m = arg(self, 0);
                    let (fast, other, (sp, _)) = self.product_slot_view(m, 2, true);
                    self.b.switch_to_block(other);
                    self.b.ins().jump(slow, &[]);
                    self.b.switch_to_block(fast);
                    let ix = self.b.ins().load(types::I64, t, sp, 0);
                    let keys = self.b.ins().load(types::I64, t, sp, 8);
                    let vals = self.b.ins().load(types::I64, t, sp, 16);
                    (ix, keys, arg(self, 1), arg(self, 2), Some((vals, arg(self, 3), object, replace)))
                } else {
                    (arg(self, 0), arg(self, 1), arg(self, 2), arg(self, 3), None)
                };
                let [p, len, room, mask] = self.map_table_words(ix, slow);
                // A map object's fields are `Vec`s by their types: only
                // their storage is tested.
                let [kp, klen, kcap] = if put.is_some() { self.vec_words(keys, slow) } else { self.array_words(keys, slow) };
                let put = match put {
                    Some((vals, v, object, replace)) => {
                        let [vp, vlen, vcap] = self.vec_words(vals, slow);
                        let mut shared = None;
                        for a in [ix, keys, vals] {
                            let bits = self.b.ins().uload8(types::I64, t, a, machine::HEADER_FLAGS_OFFSET);
                            let bit = self.b.ins().band_imm_u(bits, i64::from(machine::FLAG_SHARED));
                            shared = Some(match shared {
                                Some(s) => self.b.ins().bor(s, bit),
                                None => bit,
                            });
                        }
                        let own = self.b.create_block();
                        self.b.ins().brif(shared.expect("three fields"), slow, &[], own, &[]);
                        self.b.switch_to_block(own);
                        Some((vals, v, object, replace, vp, vlen, vcap))
                    }
                    None => None,
                };
                let start = self.map_home(h, mask);
                // The probe: `(cell, cells seen, first tombstone or -1)`.
                let (head, body, cand, next, empty) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
                for blk in [head, next] {
                    for _ in 0..3 {
                        self.b.append_block_param(blk, types::I64);
                    }
                }
                let (zero, none) = (self.b.ins().iconst(types::I64, 0), self.b.ins().iconst(types::I64, -1));
                self.b.ins().jump(head, &[start.into(), zero.into(), none.into()]);
                self.b.switch_to_block(head);
                let [i, seen, tomb] = [0, 1, 2].map(|n| self.b.block_params(head)[n]);
                let cells = self.b.ins().iadd_imm_s(mask, 1);
                // A table full of tombstones and keys: the native's answer.
                let full = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, seen, cells);
                self.b.ins().brif(full, slow, &[], body, &[]);
                self.b.switch_to_block(body);
                let off = self.b.ins().ishl_imm_u(i, 4);
                let cell = self.b.ins().iadd(p, off);
                let r = self.b.ins().load(types::I64, t, cell, hdr + 8);
                let (nonempty, live_b, tomb_b) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().brif(r, nonempty, &[], empty, &[]);
                self.b.switch_to_block(nonempty);
                let pos = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, r, 0);
                self.b.ins().brif(pos, live_b, &[], tomb_b, &[]);
                // A tombstone: remember the first, for an insert to reuse.
                self.b.switch_to_block(tomb_b);
                let first = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, tomb, 0);
                let tomb2 = self.b.ins().select(first, i, tomb);
                self.b.ins().jump(next, &[i.into(), seen.into(), tomb2.into()]);
                self.b.switch_to_block(live_b);
                let ch = self.b.ins().load(types::I64, t, cell, hdr);
                let same_h = self.b.ins().icmp(IntCC::Equal, ch, h);
                let s = self.b.ins().iadd_imm_s(r, -1);
                let in_keys = self.b.ins().icmp(IntCC::UnsignedLessThan, s, klen);
                let try_key = self.b.ins().band(same_h, in_keys);
                self.b.ins().brif(try_key, cand, &[], next, &[i.into(), seen.into(), tomb.into()]);
                self.b.switch_to_block(cand);
                let koff = self.b.ins().ishl_imm_u(s, 3);
                let kaddr = self.b.ins().iadd(kp, koff);
                let kw = self.b.ins().load(types::I64, t, kaddr, 0);
                let eq = self.b.ins().icmp(IntCC::Equal, kw, k);
                let hit = self.b.create_block();
                self.b.append_block_param(hit, types::I64);
                if string {
                    let cmp = self.b.create_block();
                    self.b.ins().brif(eq, hit, &[s.into()], cmp, &[]);
                    self.b.switch_to_block(cmp);
                    let call = self.b.ins().call(self.map_str_eq, &[kw, k]);
                    let same = self.b.inst_results(call)[0];
                    self.b.ins().brif(same, hit, &[s.into()], next, &[i.into(), seen.into(), tomb.into()]);
                } else {
                    self.b.ins().brif(eq, hit, &[s.into()], next, &[i.into(), seen.into(), tomb.into()]);
                }
                self.b.switch_to_block(hit);
                let found = self.b.block_params(hit)[0];
                if let Some((_, v, object, true, vp, vlen, _)) = put {
                    // A found key's value replaced in place, as `StoreIndex`
                    // stores it: the new word gains an owner, the old loses one.
                    let inb = self.b.ins().icmp(IntCC::UnsignedLessThan, found, vlen);
                    let store = self.b.create_block();
                    self.b.ins().brif(inb, store, &[], slow, &[]);
                    self.b.switch_to_block(store);
                    let off = self.b.ins().ishl_imm_u(found, 3);
                    let at = self.b.ins().iadd(vp, off);
                    let old = self.b.ins().load(types::I64, t, at, 0);
                    self.b.ins().store(t, v, at, 0);
                    if object {
                        self.emit_rc(v, false);
                        self.emit_rc(old, true);
                    }
                }
                self.b.ins().jump(join, &[found.into()]);
                self.b.switch_to_block(next);
                let [ni, nseen, ntomb] = [0, 1, 2].map(|n| self.b.block_params(next)[n]);
                let ni = self.b.ins().iadd_imm_s(ni, 1);
                let ni = self.b.ins().band(ni, mask);
                let nseen = self.b.ins().iadd_imm_s(nseen, 1);
                self.b.ins().jump(head, &[ni.into(), nseen.into(), ntomb.into()]);
                // An empty cell: the key is absent.
                self.b.switch_to_block(empty);
                if !upsert {
                    let none = self.b.ins().iconst(types::I64, -1);
                    self.b.ins().jump(join, &[none.into()]);
                } else {
                    // Register slot `n` (the slot count) in the first
                    // tombstone or this empty cell, as `map_table::add`
                    // does, when that needs no growth and the storage has
                    // room for the slot's word; else the native.
                    let used = self.b.ins().load(types::I64, t, p, 8);
                    let reuse = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, tomb, 0);
                    let used1 = self.b.ins().iadd_imm_s(used, 1);
                    let twice = self.b.ins().ishl_imm_u(used1, 1);
                    let no_grow = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, twice, cells);
                    let has_room = self.b.ins().icmp(IntCC::UnsignedLessThan, len, room);
                    let ok = self.b.ins().band(no_grow, has_room);
                    // `map_put` appends the key and the value too: both
                    // `Vec`s with room, and as long as the table's slots.
                    let ok = match put {
                        Some((_, _, _, _, _, vlen, vcap)) => {
                            let tail = self.b.ins().ishl_imm_u(cells, 1);
                            let tail = self.b.ins().iadd_imm_s(tail, machine::map_table::HEADER as i64);
                            let n = self.b.ins().isub(len, tail);
                            let kroom = self.b.ins().icmp(IntCC::UnsignedLessThan, klen, kcap);
                            let vroom = self.b.ins().icmp(IntCC::UnsignedLessThan, vlen, vcap);
                            let ksame = self.b.ins().icmp(IntCC::Equal, klen, n);
                            let vsame = self.b.ins().icmp(IntCC::Equal, vlen, n);
                            let ok = self.b.ins().band(ok, kroom);
                            let ok = self.b.ins().band(ok, vroom);
                            let ok = self.b.ins().band(ok, ksame);
                            self.b.ins().band(ok, vsame)
                        }
                        None => ok,
                    };
                    let write = self.b.create_block();
                    self.b.ins().brif(ok, write, &[], slow, &[]);
                    self.b.switch_to_block(write);
                    let at = self.b.ins().select(reuse, tomb, i);
                    let tail = self.b.ins().ishl_imm_u(cells, 1);
                    let tail = self.b.ins().iadd_imm_s(tail, machine::map_table::HEADER as i64);
                    let n = self.b.ins().isub(len, tail);
                    let off = self.b.ins().ishl_imm_u(at, 4);
                    let cell = self.b.ins().iadd(p, off);
                    self.b.ins().store(t, h, cell, hdr);
                    let r = self.b.ins().iadd_imm_s(n, 1);
                    self.b.ins().store(t, r, cell, hdr + 8);
                    // A tombstone reused keeps the count of cells in use.
                    let one = self.b.ins().iconst(types::I64, 1);
                    let add = self.b.ins().select(reuse, zero, one);
                    let used2 = self.b.ins().iadd(used, add);
                    self.b.ins().store(t, used2, p, 8);
                    let live = self.b.ins().load(types::I64, t, p, 16);
                    let live = self.b.ins().iadd_imm_s(live, 1);
                    self.b.ins().store(t, live, p, 16);
                    let loff = self.b.ins().ishl_imm_u(len, 3);
                    let end = self.b.ins().iadd(p, loff);
                    self.b.ins().store(t, at, end, 0);
                    let len1 = self.b.ins().iadd_imm_s(len, 1);
                    self.b.ins().store(t, len1, ix, l.len_at as i32);
                    if let Some((vals, v, object, _, vp, vlen, _)) = put {
                        // The key and the value appended, as `ArrayPush`
                        // appends: each gains an owner when it may name one.
                        for (a, ap, alen, w, counted) in [(keys, kp, klen, k, string), (vals, vp, vlen, v, object)] {
                            let off = self.b.ins().ishl_imm_u(alen, 3);
                            let at = self.b.ins().iadd(ap, off);
                            self.b.ins().store(t, w, at, 0);
                            let n1 = self.b.ins().iadd_imm_s(alen, 1);
                            self.b.ins().store(t, n1, a, l.len_at as i32);
                            let one = self.b.ins().iconst(types::I8, 1);
                            self.b.ins().store(t, one, a, l.refs_at as i32);
                            if counted {
                                self.emit_rc(w, false);
                            }
                        }
                    }
                    let res = self.b.ins().isub(none, n);
                    self.b.ins().jump(join, &[res.into()]);
                }
            }
        }
        self.b.set_cold_block(slow);
        self.b.switch_to_block(slow);
        let w = self.host_call(f, native_id, layout, args);
        self.drop_views();
        self.b.ins().jump(join, &[w.into()]);
        self.b.switch_to_block(join);
        let w = self.b.block_params(join)[0];
        // An upsert's table may have grown a word in place: its views (and
        // any other name's of the same storage) are taken again.
        if matches!(op, MapOp::Upsert { .. } | MapOp::Put { .. }) {
            self.drop_views();
        }
        self.set(f, dest, w);
    }

    /// `s[i]` read in place: fill `s`'s byte view when it is not known,
    /// then load the byte when `i` is in range and jump to the returned join
    /// block with it. Out of range (or not a string object) continues in a
    /// cold block, current on return, where the caller emits the host call
    /// and jumps to the join with its word.
    /// `string_slice_bytes(s, a, e)` in place for a result of up to
    /// `machine::native::string_alloc().max` bytes, as the host native
    /// copies one (`StrData::slice`): when `0 <= a <= e <= s.len()`, both
    /// ends are on character boundaries and inline allocation is open, a
    /// young inline string is bumped or popped from the string class's
    /// cursor, filled from its image and the bytes, and counted, and its
    /// word (an `Ok` of a niche result) goes to the returned join block.
    /// Anything else (out of range or mid-character, which the host clamps
    /// or reports, a longer result, a closed gate, a spent chunk) is left
    /// in the current block for the host call.
    fn slice_in_place(&mut self, f: &MirFunc, args: &[ValueId]) -> Block {
        let t = MemFlagsData::trusted();
        let sa = machine::native::string_alloc();
        let (p, n) = self.str_bytes(f, args[0]);
        let a = self.get(f, args[1]);
        let a = self.coerce(a, types::I64);
        let e = self.get(f, args[2]);
        let e = self.coerce(e, types::I64);
        let join = self.b.create_block();
        self.b.append_block_param(join, types::I64);
        let slow = self.b.create_block();
        let blocks: Vec<Block> = (0..4).map(|_| self.b.create_block()).collect();
        let [ranged, start_in, start_ok, end_in] = blocks[..] else { unreachable!() };
        // `a <= e <= n` unsigned (so `a >= 0`), and at most `max` bytes.
        let lo = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, a, e);
        let hi = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, e, n);
        let both = self.b.ins().band(lo, hi);
        self.b.ins().brif(both, ranged, &[], slow, &[]);
        self.b.switch_to_block(ranged);
        let len = self.b.ins().isub(e, a);
        let short = self.b.ins().icmp_imm_u(IntCC::UnsignedLessThanOrEqual, len, i64::from(sa.max));
        self.b.ins().brif(short, start_in, &[], slow, &[]);
        // An end is on a boundary at the string's end or before a byte that
        // does not continue a character (`0b10xx_xxxx`).
        let boundary = |this: &mut Self, at: cranelift_codegen::ir::Value, ok: Block, test: Block| {
            let end = this.b.ins().icmp(IntCC::Equal, at, n);
            this.b.ins().brif(end, ok, &[], test, &[]);
            this.b.switch_to_block(test);
            let byte_at = this.b.ins().iadd(p, at);
            let byte = this.b.ins().uload8(types::I64, t, byte_at, 0);
            let top = this.b.ins().band_imm_u(byte, 0xc0);
            let cont = this.b.ins().icmp_imm_s(IntCC::Equal, top, 0x80);
            this.b.ins().brif(cont, slow, &[], ok, &[]);
        };
        self.b.switch_to_block(start_in);
        let start_test = self.b.create_block();
        boundary(self, a, start_ok, start_test);
        self.b.switch_to_block(start_ok);
        let end_test = self.b.create_block();
        boundary(self, e, end_in, end_test);
        self.b.switch_to_block(end_in);
        let from = self.b.ins().iadd(p, a);
        self.inline_string(from, len, join, slow);
        self.b.set_cold_block(slow);
        self.b.switch_to_block(slow);
        join
    }

    /// A young inline string of the `len` bytes at `from` (at most
    /// [`machine::native::StringAlloc::max`], valid UTF-8: the caller
    /// checked both), made from the string class's bump chunk and handed to
    /// `join`; `slow` when inline allocation is gated off or the chunk is
    /// spent with no slot free. Ends in no block.
    fn inline_string(&mut self, from: cranelift_codegen::ir::Value, len: cranelift_codegen::ir::Value, join: Block, slow: Block) {
        use machine::native::{BUMP_BYTES, BUMP_END, BUMP_FREE, BUMP_FREE_LINK, BUMP_NEXT, COUNTS_ALLOC_BYTES, COUNTS_HEADER_XOR, COUNTS_INLINE_GATE, COUNTS_LIVE_COUNT, NATIVE_CTX_ALLOC_BUMPS, NATIVE_CTX_ALLOC_COUNTS};
        let t = MemFlagsData::trusted();
        let sa = machine::native::string_alloc();
        let blocks: Vec<Block> = (0..5).map(|_| self.b.create_block()).collect();
        let [gated, bump, spent, popped, fill] = blocks[..] else { unreachable!() };
        self.b.append_block_param(fill, types::I64);
        let counts = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_ALLOC_COUNTS);
        let gate = self.b.ins().load(types::I64, t, counts, COUNTS_INLINE_GATE);
        self.b.ins().brif(gate, gated, &[], slow, &[]);
        self.b.switch_to_block(gated);
        let bumps = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_ALLOC_BUMPS);
        let cursor = self.b.ins().iadd_imm_s(bumps, i64::from(sa.class) * i64::from(BUMP_BYTES));
        let at = self.b.ins().load(types::I64, t, cursor, BUMP_NEXT);
        let end = self.b.ins().load(types::I64, t, cursor, BUMP_END);
        let next = self.b.ins().iadd_imm_s(at, i64::from(sa.slot));
        let fits = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, next, end);
        self.b.ins().brif(fits, bump, &[], spent, &[]);
        self.b.switch_to_block(bump);
        self.b.ins().store(t, next, cursor, BUMP_NEXT);
        self.b.ins().jump(fill, &[at.into()]);
        // A spent chunk: a slot freed since, else the host call (which
        // refills the chunk).
        self.b.switch_to_block(spent);
        let free = self.b.ins().load(types::I64, t, cursor, BUMP_FREE);
        self.b.ins().brif(free, popped, &[], slow, &[]);
        self.b.switch_to_block(popped);
        let link = self.b.ins().load(types::I64, t, free, BUMP_FREE_LINK);
        self.b.ins().store(t, link, cursor, BUMP_FREE);
        self.b.ins().jump(fill, &[free.into()]);
        self.b.switch_to_block(fill);
        let o = self.b.block_params(fill)[0];
        for (i, &img) in sa.image.iter().enumerate() {
            let x = if i == 0 {
                // A steal epoch's mutator makes it old and stamps its epoch
                // (`HeapCounts::header_xor`, zero else).
                let xor = self.b.ins().load(types::I64, t, counts, COUNTS_HEADER_XOR);
                self.b.ins().bxor_imm_s(xor, img as i64)
            } else {
                self.b.ins().iconst(types::I64, img as i64)
            };
            self.b.ins().store(t, x, o, (i * 8) as i32);
        }
        let len8 = self.b.ins().ireduce(types::I8, len);
        self.b.ins().store(t, len8, o, sa.len_at as i32);
        // The bytes, one at a time (at most `max`): reading past the
        // source's last byte could leave its mapping.
        let (copy, copied) = (self.b.create_block(), self.b.create_block());
        self.b.append_block_param(copy, types::I64);
        let zero = self.b.ins().iconst(types::I64, 0);
        let none = self.b.ins().icmp_imm_s(IntCC::Equal, len, 0);
        self.b.ins().brif(none, copied, &[], copy, &[zero.into()]);
        self.b.switch_to_block(copy);
        let k = self.b.block_params(copy)[0];
        let from = self.b.ins().iadd(from, k);
        let byte = self.b.ins().uload8(types::I64, t, from, 0);
        let byte = self.b.ins().ireduce(types::I8, byte);
        let to = self.b.ins().iadd(o, k);
        self.b.ins().store(t, byte, to, sa.bytes_at as i32);
        let k1 = self.b.ins().iadd_imm_s(k, 1);
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, k1, len);
        self.b.ins().brif(more, copy, &[k1.into()], copied, &[]);
        self.b.switch_to_block(copied);
        let bytes = self.b.ins().load(types::I64, t, counts, COUNTS_ALLOC_BYTES);
        let bytes = self.b.ins().iadd_imm_s(bytes, i64::from(sa.size));
        self.b.ins().store(t, bytes, counts, COUNTS_ALLOC_BYTES);
        let live = self.b.ins().load(types::I64, t, counts, COUNTS_LIVE_COUNT);
        let live = self.b.ins().iadd_imm_s(live, 1);
        self.b.ins().store(t, live, counts, COUNTS_LIVE_COUNT);
        self.b.ins().jump(join, &[o.into()]);
    }

    /// `from_bytes` (or `from_bytes_take`) of a byte array of at most
    /// [`machine::native::StringAlloc::max`] ASCII bytes: the inline string
    /// made here ([`Self::inline_string`]), whose word is the `Ok` of the
    /// result's niche. Anything else (word storage, a longer or non-ASCII
    /// buffer, no slot) branches to the host call, after which the caller
    /// jumps to the returned join block with its result.
    fn string_from_bytes_in_place(&mut self, f: &MirFunc, buf: ValueId) -> Block {
        let t = MemFlagsData::trusted();
        let l = machine::native::array_slots();
        let sa = machine::native::string_alloc();
        let b = self.get(f, buf);
        let b = self.coerce(b, types::I64);
        let join = self.b.create_block();
        self.b.append_block_param(join, types::I64);
        let slow = self.b.create_block();
        let blocks: Vec<Block> = (0..5).map(|_| self.b.create_block()).collect();
        let [kind_b, tag_b, sized, scan, ascii] = blocks[..] else { unreachable!() };
        self.b.append_block_param(scan, types::I64);
        self.b.ins().brif(b, kind_b, &[], slow, &[]);
        self.b.switch_to_block(kind_b);
        let k = self.b.ins().uload8(types::I64, t, b, l.kind_at as i32);
        let is = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(l.kind));
        self.b.ins().brif(is, tag_b, &[], slow, &[]);
        self.b.switch_to_block(tag_b);
        let tag = self.b.ins().uload8(types::I64, t, b, l.data_at as i32);
        let is = self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(l.bytes_tag));
        self.b.ins().brif(is, sized, &[], slow, &[]);
        self.b.switch_to_block(sized);
        let p = self.b.ins().load(types::I64, t, b, l.bytes_ptr_at as i32);
        let n = self.b.ins().load(types::I64, t, b, l.bytes_len_at as i32);
        let short = self.b.ins().icmp_imm_u(IntCC::UnsignedLessThanOrEqual, n, i64::from(sa.max));
        let none = self.b.ins().icmp_imm_s(IntCC::Equal, n, 0);
        let zero = self.b.ins().iconst(types::I64, 0);
        let (go, empty) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(short, go, &[], slow, &[]);
        self.b.switch_to_block(go);
        self.b.ins().brif(none, empty, &[], scan, &[zero.into()]);
        self.b.switch_to_block(empty);
        self.b.ins().jump(ascii, &[]);
        // Every byte below 0x80: ASCII, so UTF-8 with no check of its own.
        self.b.switch_to_block(scan);
        let i = self.b.block_params(scan)[0];
        let at = self.b.ins().iadd(p, i);
        let byte = self.b.ins().uload8(types::I64, t, at, 0);
        let high = self.b.ins().band_imm_u(byte, 0x80);
        let (next, more) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(high, slow, &[], next, &[]);
        self.b.switch_to_block(next);
        let i1 = self.b.ins().iadd_imm_s(i, 1);
        let left = self.b.ins().icmp(IntCC::UnsignedLessThan, i1, n);
        self.b.ins().brif(left, more, &[], ascii, &[]);
        self.b.switch_to_block(more);
        self.b.ins().jump(scan, &[i1.into()]);
        self.b.switch_to_block(ascii);
        self.inline_string(p, n, join, slow);
        self.b.set_cold_block(slow);
        self.b.switch_to_block(slow);
        join
    }

    fn byte_in_place(&mut self, f: &MirFunc, s: ValueId, i: ValueId) -> Block {
        let i = self.get(f, i);
        let i = self.coerce(i, types::I64);
        let (p, n) = self.str_bytes(f, s);
        let (read, slow, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.append_block_param(join, types::I64);
        let inb = self.b.ins().icmp(IntCC::UnsignedLessThan, i, n);
        self.b.ins().brif(inb, read, &[], slow, &[]);
        self.b.switch_to_block(read);
        let at = self.b.ins().iadd(p, i);
        let byte = self.b.ins().uload8(types::I64, MemFlagsData::trusted(), at, 0);
        self.b.ins().jump(join, &[byte.into()]);
        self.b.set_cold_block(slow);
        self.b.switch_to_block(slow);
        join
    }

    /// Branch on whether `array[i]` is in its view: `(in view, otherwise,
    /// join)`. The join block takes one word when the caller passes one.
    fn in_view(&mut self, f: &MirFunc, array: ValueId, i: cranelift_codegen::ir::Value, word: bool) -> (Block, Block, Block) {
        self.in_view_as(f, array, i, word, false)
    }

    /// [`Self::in_view`]; a `store` is in place only through a plain view
    /// (`machine::VIEW_PLAIN`): a tracked one's stores go through
    /// the runtime, which marks the array's card table.
    fn in_view_as(&mut self, f: &MirFunc, array: ValueId, i: cranelift_codegen::ir::Value, word: bool, store: bool) -> (Block, Block, Block) {
        let fast = self.b.create_block();
        let slow = self.b.create_block();
        let join = self.b.create_block();
        if word {
            self.b.append_block_param(join, types::I64);
        }
        let c = self.view_has(array, i, store);
        if !self.maybe_dropped.contains(&array) {
            self.b.ins().brif(c, fast, &[], slow, &[]);
            return (fast, slow, join);
        }
        let (miss, refill) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(c, fast, &[], miss, &[]);
        // Not in the view: a view dropped since it was taken (a call or a
        // push ran) is taken again, in a cold block, and checked once more.
        self.b.switch_to_block(miss);
        let (ok, _, _) = self.views[&array];
        let state = self.b.use_var(ok);
        let unknown = self.b.ins().icmp_imm_s(IntCC::Equal, state, VIEW_UNKNOWN);
        self.b.ins().brif(unknown, refill, &[], slow, &[]);
        self.b.set_cold_block(refill);
        self.b.switch_to_block(refill);
        self.fill_view(f, array);
        let c = self.view_has(array, i, store);
        self.b.ins().brif(c, fast, &[], slow, &[]);
        (fast, slow, join)
    }

    /// Whether element `i` of `array` is in its word view (for a store:
    /// a plain view's).
    fn view_has(&mut self, array: ValueId, i: cranelift_codegen::ir::Value, store: bool) -> cranelift_codegen::ir::Value {
        let (ok, _, len) = self.views[&array];
        let ok = self.b.use_var(ok);
        let len = self.b.use_var(len);
        let inb = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        let has = if store {
            self.b.ins().icmp_imm_s(IntCC::Equal, ok, machine::VIEW_PLAIN as i64)
        } else {
            self.view_known(ok)
        };
        self.b.ins().band(inb, has)
    }

    /// Whether word-view state `ok` is a view ([`machine::VIEW_PLAIN`]):
    /// above `0` (none) and [`VIEW_UNKNOWN`].
    fn view_known(&mut self, ok: cranelift_codegen::ir::Value) -> cranelift_codegen::ir::Value {
        self.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, ok, 0)
    }

    /// Where slot `index` of `o` is when `o` is a product object that has
    /// it (`machine::native::product_slots`): an instance's field or an
    /// enum's payload word (a tuple's item), one store at one offset.
    /// Branches to the first block, with the slot's address as its
    /// parameter, or to the second. A `store` takes instances only.
    /// An inline `RcInc` / `RcDec` of `s`, a word that is an object or null
    /// (a tag bit allowed): the header count is read and written here, and
    /// the runtime is called only for a decrement that may release the
    /// object. In a steal epoch only an object born in it is counted here
    /// (atomically); nothing for an immortal object. Leaves the builder
    /// after it.
    /// Branch to the returned block when the word `s` (of a type that may
    /// hold a number) may name an object: 8-aligned but for a `Result`'s
    /// `Err` bit, and inside the heap's chunk span. Anything else goes on to
    /// the join block, which the caller switches to after the first.
    fn maybe_object(&mut self, s: cranelift_codegen::ir::Value) -> (Block, Block) {
        let t = MemFlagsData::trusted();
        let (aligned, has_heap, low, call, join) =
            (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
        let odd = self.b.ins().band_imm_u(s, 6);
        self.b.ins().brif(odd, join, &[], aligned, &[]);
        self.b.switch_to_block(aligned);
        let heap = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_HEAP);
        self.b.ins().brif(heap, has_heap, &[], join, &[]);
        self.b.switch_to_block(has_heap);
        let o = self.b.ins().band_imm_u(s, !1i64);
        let lo = self.b.ins().load(types::I64, t, heap, machine::HEAP_SLAB_LO_OFFSET);
        let below = self.b.ins().icmp(IntCC::UnsignedLessThan, o, lo);
        self.b.ins().brif(below, join, &[], low, &[]);
        self.b.switch_to_block(low);
        let hi = self.b.ins().load(types::I64, t, heap, machine::HEAP_SLAB_HI_OFFSET);
        let inside = self.b.ins().icmp(IntCC::UnsignedLessThan, o, hi);
        self.b.ins().brif(inside, call, &[], join, &[]);
        (call, join)
    }

    fn emit_rc(&mut self, s: cranelift_codegen::ir::Value, dec: bool) {
        let t = MemFlagsData::trusted();
        // A word of an object type may still name none (a function value's
        // code offset, a static's): only one in the heap's span is counted.
        let (lock_b, join) = self.maybe_object(s);
        let (count_b, change, logged) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.switch_to_block(lock_b);
        let o = self.b.ins().band_imm_u(s, !1i64);
        let heap = self.b.ins().load(types::I64, t, self.ctx, NATIVE_CTX_HEAP);
        let lock = self.b.ins().load(types::I64, t, heap, machine::HEAP_ALLOC_LOCK_OFFSET);
        self.b.ins().brif(lock, logged, &[], count_b, &[]);
        // In a steal epoch an object born in it is counted in place, with
        // an atomic change (its mutators share it); the runtime logs the
        // change to any other (plan step 8, #2927).
        let (born, runtime, shared) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.set_cold_block(logged);
        self.b.switch_to_block(logged);
        let g = self.b.ins().uload8(types::I64, t, heap, machine::HEAP_EPOCH_GEN_OFFSET);
        let e = self.b.ins().uload8(types::I64, t, o, machine::HEADER_EPOCH_OFFSET);
        let same = self.b.ins().icmp(IntCC::Equal, g, e);
        self.b.ins().brif(same, born, &[], runtime, &[]);
        self.b.switch_to_block(born);
        let c = self.b.ins().uload32(t, o, machine::HEADER_COUNT_OFFSET);
        // Immortal, or (a decrement) already at zero: unchanged, as the
        // runtime's [`machine::memory::Heap::rc_dec_obj`] leaves it.
        let fixed = self.b.ins().icmp_imm_s(IntCC::Equal, c, i64::from(machine::COUNT_IMMORTAL));
        let fixed = if dec {
            let zero = self.b.ins().icmp_imm_s(IntCC::Equal, c, 0);
            self.b.ins().bor(fixed, zero)
        } else {
            fixed
        };
        self.b.ins().brif(fixed, join, &[], shared, &[]);
        self.b.switch_to_block(shared);
        let at = self.b.ins().iadd_imm_s(o, i64::from(machine::HEADER_COUNT_OFFSET));
        let one = self.b.ins().iconst(types::I32, 1);
        let op = if dec { AtomicRmwOp::Sub } else { AtomicRmwOp::Add };
        let was = self.b.ins().atomic_rmw(types::I32, t, op, at, one);
        if dec {
            // Its last owner went: the epoch's end releases it.
            let zero_b = self.b.create_block();
            let last = self.b.ins().icmp_imm_s(IntCC::Equal, was, 1);
            self.b.ins().brif(last, zero_b, &[], join, &[]);
            self.b.set_cold_block(zero_b);
            self.b.switch_to_block(zero_b);
            let flag = self.b.ins().iconst(types::I64, 2);
            self.b.ins().call(self.rc, &[self.ctx, s, flag]);
            self.b.ins().jump(join, &[]);
        } else {
            self.b.ins().jump(join, &[]);
        }
        self.b.set_cold_block(runtime);
        self.b.switch_to_block(runtime);
        let flag = self.b.ins().iconst(types::I64, i64::from(dec));
        self.b.ins().call(self.rc, &[self.ctx, s, flag]);
        self.b.ins().jump(join, &[]);
        self.b.switch_to_block(count_b);
        let c = self.b.ins().uload32(t, o, machine::HEADER_COUNT_OFFSET);
        let immortal = self.b.ins().icmp_imm_s(IntCC::Equal, c, i64::from(machine::COUNT_IMMORTAL));
        if dec {
            let (test, slow) = (self.b.create_block(), self.b.create_block());
            self.b.ins().brif(immortal, join, &[], test, &[]);
            self.b.switch_to_block(test);
            // At one, the runtime takes the last owner and frees.
            let last = self.b.ins().icmp_imm_u(IntCC::UnsignedLessThanOrEqual, c, 1);
            self.b.ins().brif(last, slow, &[], change, &[]);
            self.b.switch_to_block(change);
            let n = self.b.ins().iadd_imm_s(c, -1);
            self.b.ins().istore32(t, n, o, machine::HEADER_COUNT_OFFSET);
            self.b.ins().jump(join, &[]);
            // The last owner: the module's `coil_rc_last` frees a young
            // string in place, anything else through the runtime. One call
            // here, out of line, as the runtime call it stands for was.
            self.b.set_cold_block(slow);
            self.b.switch_to_block(slow);
            self.b.ins().call(self.rc_last, &[self.ctx, s]);
            self.b.ins().jump(join, &[]);
        } else {
            self.b.ins().brif(immortal, join, &[], change, &[]);
            self.b.switch_to_block(change);
            let n = self.b.ins().iadd_imm_s(c, 1);
            self.b.ins().istore32(t, n, o, machine::HEADER_COUNT_OFFSET);
            self.b.ins().jump(join, &[]);
        }
        self.b.switch_to_block(join);
    }

    fn product_slot(&mut self, o: cranelift_codegen::ir::Value, index: u32, store: bool) -> (Block, Block) {
        let (fast, slow, _) = self.product_slot_view(o, index, store);
        (fast, slow)
    }

    /// [`Self::product_slot`] of the object `obj` (word `o`): through its
    /// view ([`Self::product_views`]) when it has one, which the in-place
    /// test fills for the next access when it is not taken yet, or through
    /// what an earlier read in the block found ([`Self::block_views`]).
    fn field_slot(&mut self, obj: ValueId, o: cranelift_codegen::ir::Value, index: u32, store: bool) -> (Block, Block) {
        let Some(&(ptr, len)) = self.product_views.get(&obj) else {
            if let Some(&(p, n)) = self.block_views.get(&obj) {
                let (fast, slow) = (self.b.create_block(), self.b.create_block());
                self.b.append_block_param(fast, types::I64);
                let has = self.b.ins().icmp_imm_u(IntCC::UnsignedGreaterThan, n, i64::from(index));
                let at = self.b.ins().iadd_imm_s(p, i64::from(index) * 8);
                self.b.ins().brif(has, fast, &[at.into()], slow, &[]);
                return (fast, slow);
            }
            let (fast, slow, view) = self.product_slot_view(o, index, store);
            // Only a read's: its other way bails, so the rest of the block
            // is reached through `fast`, where the view is defined.
            if !store {
                self.block_views.insert(obj, view);
            }
            return (fast, slow);
        };
        let (have, take, fast) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.append_block_param(fast, types::I64);
        let n = self.b.use_var(len);
        let has = self.b.ins().icmp_imm_u(IntCC::UnsignedGreaterThan, n, i64::from(index));
        self.b.ins().brif(has, have, &[], take, &[]);
        self.b.switch_to_block(have);
        let p = self.b.use_var(ptr);
        let at = self.b.ins().iadd_imm_s(p, i64::from(index) * 8);
        self.b.ins().jump(fast, &[at.into()]);
        self.b.switch_to_block(take);
        let (found, slow, (vp, vn)) = self.product_slot_view(o, index, store);
        self.b.switch_to_block(found);
        self.b.def_var(ptr, vp);
        self.b.def_var(len, vn);
        let at = self.b.block_params(found)[0];
        self.b.ins().jump(fast, &[at.into()]);
        (fast, slow)
    }

    /// [`Self::product_slot`], with the object's `(pointer, length)` of
    /// its slot words as well, defined where `fast` is entered.
    fn product_slot_view(
        &mut self,
        o: cranelift_codegen::ir::Value,
        index: u32,
        store: bool,
    ) -> (Block, Block, (cranelift_codegen::ir::Value, cranelift_codegen::ir::Value)) {
        let l = machine::native::product_slots();
        let flags = MemFlagsData::trusted();
        let (fast, slow) = (self.b.create_block(), self.b.create_block());
        self.b.append_block_param(fast, types::I64);
        let (kind, storage, inline, spill_test, spill, check) =
            (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.append_block_param(check, types::I64);
        self.b.append_block_param(check, types::I64);
        self.b.ins().brif(o, kind, &[], slow, &[]);
        self.b.switch_to_block(kind);
        let k = self.b.ins().uload8(types::I64, flags, o, l.kind_at as i32);
        if store {
            let is = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(l.kind));
            self.b.ins().brif(is, storage, &[], slow, &[]);
        } else {
            // The two kinds differ in one bit: one test for both.
            let bit = l.kind ^ l.enum_kind;
            let either = if bit.is_power_of_two() {
                let k = self.b.ins().band_imm_u(k, i64::from(!bit));
                self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(l.kind & !bit))
            } else {
                let is = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(l.kind));
                let is_enum = self.b.ins().icmp_imm_s(IntCC::Equal, k, i64::from(l.enum_kind));
                self.b.ins().bor(is, is_enum)
            };
            self.b.ins().brif(either, storage, &[], slow, &[]);
        }
        self.b.switch_to_block(storage);
        let tag = self.b.ins().uload8(types::I64, flags, o, l.storage_at as i32);
        let is_inline = self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(l.inline_tag));
        self.b.ins().brif(is_inline, inline, &[], spill_test, &[]);
        self.b.switch_to_block(inline);
        let len = self.b.ins().uload8(types::I64, flags, o, l.inline_len_at as i32);
        let ptr = self.b.ins().iadd_imm_s(o, i64::from(l.inline_slots_at));
        self.b.ins().jump(check, &[ptr.into(), len.into()]);
        self.b.switch_to_block(spill_test);
        let is_spill = self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::from(l.spill_tag));
        self.b.ins().brif(is_spill, spill, &[], slow, &[]);
        self.b.switch_to_block(spill);
        let ptr = self.b.ins().load(types::I64, flags, o, l.spill_ptr_at as i32);
        let len = self.b.ins().load(types::I64, flags, o, l.spill_len_at as i32);
        self.b.ins().jump(check, &[ptr.into(), len.into()]);
        self.b.switch_to_block(check);
        let (ptr, len) = (self.b.block_params(check)[0], self.b.block_params(check)[1]);
        let has = self.b.ins().icmp_imm_u(IntCC::UnsignedGreaterThan, len, i64::from(index));
        let at = self.b.ins().iadd_imm_s(ptr, i64::from(index) * 8);
        self.b.ins().brif(has, fast, &[at.into()], slow, &[]);
        (fast, slow, (ptr, len))
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
        // A viewed φ takes its source's word view along the edge (no
        // runtime call per iteration); with none, its view is taken at its
        // first access ([`drop_transfer`] marks it maybe dropped).
        let mut view_moves = Vec::new();
        for &(dest, v) in &moves {
            let Some(&(ok, ptr, len)) = self.views.get(&dest) else { continue };
            let from = match self.views.get(&v) {
                Some(&(o, p, l)) => {
                    let (o, p, l) = (self.b.use_var(o), self.b.use_var(p), self.b.use_var(l));
                    (o, p, l)
                }
                None => {
                    let unknown = self.b.ins().iconst(types::I64, VIEW_UNKNOWN);
                    let zero = self.b.ins().iconst(types::I64, 0);
                    (unknown, zero, zero)
                }
            };
            view_moves.push(((ok, ptr, len), from));
        }
        for ((dest, _), x) in moves.into_iter().zip(vals) {
            self.set(f, dest, x);
        }
        for ((ok, ptr, len), (o, p, l)) in view_moves {
            self.b.def_var(ok, o);
            self.b.def_var(ptr, p);
            self.b.def_var(len, l);
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
            Terminator::Return { lo, hi, more } => {
                self.keep_roots(f);
                let x = match lo {
                    Some(v) => self.get(f, *v),
                    None => self.b.ins().iconst(types::I64, 0),
                };
                let x = self.coerce(x, self.ret);
                if more.len() != self.ret_more.len() {
                    return Err("return width differs from the signature".into());
                }
                match (hi, self.ret_hi) {
                    (None, None) => {
                        self.b.ins().return_(&[x]);
                    }
                    (Some(h), Some(t)) => {
                        let y = self.get(f, *h);
                        let y = self.coerce(y, t);
                        let mut words = vec![x, y];
                        for (w, t) in more.iter().zip(self.ret_more.clone()) {
                            let z = self.get(f, *w);
                            words.push(self.coerce(z, t));
                        }
                        self.b.ins().return_(&words);
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
                        let view = self.payload_views.get(scrutinee).copied();
                        let tl = self.enum_tag_word(s, view);
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

/// The objects `f` reaches fields of (a field read, a field store, a
/// field's copy test) at two or more places, in two or more blocks or with a
/// store or a copy test among them, for [`Lower::product_views`]. Reads in
/// one block share what the first one found ([`Lower::block_views`]).
fn field_objects(f: &MirFunc) -> Vec<ValueId> {
    // Each object's blocks, places, and whether a store or a test is one.
    let mut uses: HashMap<ValueId, (HashSet<BlockId>, u32, bool)> = HashMap::new();
    for b in &f.blocks {
        for inst in &b.insts {
            let (object, write) = match inst {
                MirInst::ProductLoad { object, .. } => (*object, false),
                MirInst::HeapFieldStore { object, .. } | MirInst::Cow { src: object, unique: true, field: Some(_), .. } => (*object, true),
                _ => continue,
            };
            let u = uses.entry(object).or_default();
            u.0.insert(b.id);
            u.1 += 1;
            u.2 |= write;
        }
    }
    let mut out: Vec<_> = uses.into_iter().filter(|(_, (b, n, w))| b.len() >= 2 || *n >= 2 && *w).map(|(v, _)| v).collect();
    out.sort_unstable();
    out
}

/// Boxed enum values `f` reads two or more payload words of (a tag test
/// counts as one read), and never reads or writes as a class's fields:
/// the values worth a payload view.
fn payload_scrutinees(f: &MirFunc) -> Vec<ValueId> {
    let mut reads: HashMap<ValueId, u32> = HashMap::new();
    let mut fields = HashSet::new();
    for blk in &f.blocks {
        for inst in &blk.insts {
            match inst {
                MirInst::MatchPayload { scrutinee, .. } => *reads.entry(*scrutinee).or_default() += 1,
                MirInst::ProductLoad { object, .. } | MirInst::HeapFieldStore { object, .. } => {
                    fields.insert(*object);
                }
                _ => {}
            }
        }
        if let Some(Terminator::JumpIfMatch { scrutinee, .. }) = &blk.term {
            *reads.entry(*scrutinee).or_default() += 1;
        }
    }
    let mut out: Vec<_> = reads
        .into_iter()
        .filter(|&(v, n)| n >= 2 && !fields.contains(&v) && !matches!(ty_of(f, v), MirTy::NicheOpt | MirTy::NicheRes))
        .map(|(v, _)| v)
        .collect();
    out.sort_unstable();
    out
}

/// Body names compiled per call to [`jit`], for `COIL_NATIVE_STATS`.
pub fn describe(p: &NativeProgram) -> String {
    let refused: HashSet<&str> = p.refused.iter().map(|(n, _)| n.as_str()).collect();
    let mut out = format!("native: {} compiled, {} installed, {} refused", p.compiled.len(), p.entries.len(), refused.len());
    // `COIL_NATIVE_STATS=why`: each refusal's reason too.
    if std::env::var_os("COIL_NATIVE_STATS").is_some_and(|v| v == "why") {
        for (name, why) in &p.refused {
            out.push_str(&format!("\n  refused {name}: {why}"));
        }
    }
    out
}

/// Whether every operand of scan predicate `e` names one of `streams`
/// streams or `scalars` scalars.
fn lane_expr_fits(e: &compiler::native_mir::LaneExpr, streams: usize, scalars: usize) -> bool {
    use compiler::native_mir::{LaneExpr, LaneVal};
    let fits = |v: &LaneVal| match *v {
        LaneVal::Stream(k) => usize::from(k) < streams,
        LaneVal::Scalar(k) => usize::from(k) < scalars,
    };
    match e {
        LaneExpr::Const(_) => true,
        LaneExpr::Cmp { lhs, rhs, .. } => fits(lhs) && fits(rhs),
        LaneExpr::And(a, b) | LaneExpr::Or(a, b) => lane_expr_fits(a, streams, scalars) && lane_expr_fits(b, streams, scalars),
        LaneExpr::Not(a) => lane_expr_fits(a, streams, scalars),
    }
}

/// Whether a word of type `t` is an object or null by its type, so native
/// code counts it inline ([`Lower::emit_rc`]).
/// Whether a `HostInvoke` is a `string_slice_bytes` whose `Ok` result is
/// its string's word (a niche result) that native code may make in place
/// (`Lower::slice_in_place`; `COIL_NATIVE_INLINE_ALLOC=0` turns it off).
/// Whether a `from_bytes` / `from_bytes_take` call returns its result in
/// the niche layout, so a short ASCII one can be made in place
/// ([`Lower::string_from_bytes_in_place`]).
fn string_from_bytes_in_place_fits(native_id: u16, layout: u8, args: &[ValueId]) -> bool {
    (native_id == common::FROM_BYTES_ID || native_id == common::FROM_BYTES_TAKE_ID)
        && args.len() == 1
        && common::host_layout_base(u32::from(layout)) == common::HOST_ENUM_LAYOUT_RESULT_NICHE
        && !std::env::var_os("COIL_NATIVE_INLINE_ALLOC").is_some_and(|v| v == "0")
}

fn slice_in_place_fits(native_id: u16, layout: u8, args: &[ValueId]) -> bool {
    native_id == common::STRING_SLICE_BYTES_ID
        && args.len() == 3
        && common::host_layout_base(u32::from(layout)) == common::HOST_ENUM_LAYOUT_RESULT_NICHE
        && !std::env::var_os("COIL_NATIVE_INLINE_ALLOC").is_some_and(|v| v == "0")
}

fn counted_inline(t: MirTy) -> bool {
    matches!(t, MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes)
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
        b.term = Some(Terminator::Return { lo: Some(ValueId(0)), hi: None, more: Vec::new() });
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
        b.insts.push(MirInst::Call { dest: ValueId(1), dest_hi: None, dest_more: Vec::new(), target: Label(1), args: vec![ValueId(0)] });
        b.term = Some(Terminator::Return { lo: Some(ValueId(1)), hi: None, more: Vec::new() });
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

    /// `f(v0, v1)`: two `rc.inc v0`, then `rc.dec v0 if v1`; returns `v1`.
    /// `g(v0)`: `rc.dec v0`.
    fn counting_bodies() -> Vec<NativeBody> {
        let mut f = MirFunc::new("f");
        f.blocks.clear();
        f.types = vec![MirTy::HeapRef, MirTy::Bool, MirTy::Bool, MirTy::Bool, MirTy::Bool];
        f.params = vec![ValueId(0), ValueId(1)];
        f.ret_ty = Some(MirTy::Bool);
        let mut b = MirBlock::new(BlockId(0));
        b.insts.push(MirInst::RcInc { dest: ValueId(2), src: ValueId(0), guard: None });
        b.insts.push(MirInst::RcInc { dest: ValueId(3), src: ValueId(0), guard: None });
        b.insts.push(MirInst::RcDec { dest: ValueId(4), src: ValueId(0), guard: Some(ValueId(1)) });
        b.term = Some(Terminator::Return { lo: Some(ValueId(1)), hi: None, more: Vec::new() });
        f.blocks.push(b);
        let mut g = MirFunc::new("g");
        g.blocks.clear();
        g.types = vec![MirTy::HeapRef, MirTy::Bool];
        g.params = vec![ValueId(0)];
        g.ret_ty = Some(MirTy::Bool);
        let mut b = MirBlock::new(BlockId(0));
        b.insts.push(MirInst::RcDec { dest: ValueId(1), src: ValueId(0), guard: None });
        b.term = Some(Terminator::Return { lo: Some(ValueId(1)), hi: None, more: Vec::new() });
        g.blocks.push(b);
        vec![NativeBody { name: "f".into(), entry: Some(Label(0)), mir: f }, NativeBody { name: "g".into(), entry: Some(Label(1)), mir: g }]
    }

    /// The reference count ops (plan step 3): an increment, a guarded
    /// decrement that applies only when its guard is true, and a decrement
    /// of the last owner that frees the object.
    #[test]
    fn the_count_ops_move_the_count_and_the_last_one_frees() {
        let mut heap = machine::Heap::default();
        heap.set_rc_counting(true);
        heap.set_rc_frees(true);
        let (obj, _) = heap.alloc(machine::ObjString::from("counted"), machine::Object::String);
        obj.rc_inc();
        let pcs = HashMap::from([("f".to_string(), 0u32), ("g".to_string(), 1u32)]);
        let p = jit(&counting_bodies(), &pcs).expect("jit");
        assert!(p.refused.is_empty(), "refused: {:?}", p.refused);
        let entry = |name: u32| p.entries.iter().find(|e| e.pc == name).expect("entry");
        let call = |heap: &mut machine::Heap, pc: u32, args: &[u64]| {
            let marker = 0u8;
            let mut ctx = NativeCtx::new((std::ptr::addr_of!(marker) as u64).saturating_sub(NATIVE_STACK_BUDGET));
            ctx.heap = heap;
            // SAFETY: compiled for `args.len()` argument words.
            unsafe { (entry(pc).entry)(&mut ctx, args.as_ptr()) };
            assert_eq!(ctx.bail, 0);
        };
        call(&mut heap, 0, &[obj.addr(), 0]);
        assert_eq!(obj.rc_count(), 3, "the guarded decrement did not run");
        call(&mut heap, 0, &[obj.addr(), 1]);
        assert_eq!(obj.rc_count(), 4);
        let live = heap.live_object_count();
        for _ in 0..3 {
            call(&mut heap, 1, &[obj.addr()]);
        }
        assert_eq!((obj.rc_count(), heap.live_object_count()), (1, live));
        call(&mut heap, 1, &[obj.addr()]);
        assert_eq!(heap.live_object_count(), live - 1, "the last owner's decrement frees it");
    }

    /// `f(v0)`: pin `v0`, share it, pin it again and unpin twice, the
    /// inner unpin with the outer pin's word; returns the share's word, or
    /// with `second`, what the second pin said.
    fn pin_pair(second: bool) -> MirFunc {
        let mut f = MirFunc::new("f");
        f.blocks.clear();
        f.types = vec![MirTy::HeapRef, MirTy::Bool, MirTy::HeapRef, MirTy::Bool, MirTy::Bool, MirTy::Bool, MirTy::I64];
        f.params = vec![ValueId(0)];
        f.ret_ty = Some(if second { MirTy::I64 } else { MirTy::HeapRef });
        let mut b = MirBlock::new(BlockId(0));
        b.insts.push(MirInst::CowPin { dest: ValueId(1), src: ValueId(0) });
        b.insts.push(MirInst::Cow { dest: ValueId(2), src: ValueId(0), unique: false, field: None });
        b.insts.push(MirInst::CowPin { dest: ValueId(3), src: ValueId(0) });
        b.insts.push(MirInst::CowUnpin { dest: ValueId(4), src: ValueId(0), saved: ValueId(3) });
        b.insts.push(MirInst::CowUnpin { dest: ValueId(5), src: ValueId(0), saved: ValueId(1) });
        b.insts.push(MirInst::Cast { dest: ValueId(6), src: ValueId(3), kind: compiler::native_mir::MirCastKind::Word, to: MirTy::I64 });
        b.term = Some(Terminator::Return { lo: Some(ValueId(if second { 6 } else { 2 })), hi: None, more: Vec::new() });
        f.blocks.push(b);
        f
    }

    /// The pin ops and the inline share's pinned path (archive 8.2): a share
    /// of a pinned object hands out a copy, the pin nests, and the last
    /// unpin clears it.
    #[test]
    fn a_share_of_a_pinned_object_copies_it_and_the_pins_nest() {
        let mut heap = machine::Heap::default();
        let (obj, _) = heap.alloc(machine::ObjInstance::with_slots(5, vec![common::Value::from(7i64)]), machine::Object::Instance);
        for second in [false, true] {
            let bodies = vec![NativeBody { name: "f".into(), entry: Some(Label(0)), mir: pin_pair(second) }];
            let pcs = HashMap::from([("f".to_string(), 0u32)]);
            let p = jit(&bodies, &pcs).expect("jit");
            assert!(p.refused.is_empty(), "refused: {:?}", p.refused);
            let marker = 0u8;
            let mut ctx = NativeCtx::new((std::ptr::addr_of!(marker) as u64).saturating_sub(NATIVE_STACK_BUDGET));
            ctx.heap = &mut heap;
            // SAFETY: compiled for one argument word.
            let out = unsafe { (p.entries[0].entry)(&mut ctx, [obj.addr()].as_ptr()) };
            assert_eq!(ctx.bail, 0);
            if second {
                assert_eq!(out, 1, "the second pin finds the first");
            } else {
                assert_ne!(out, obj.addr(), "the share hands out a copy");
                let copy = heap.find_object_by_addr(out).expect("the copy");
                assert!(!copy.is_shared() && !copy.is_pinned());
            }
            assert!(!obj.is_shared(), "a pinned object is never marked");
            assert!(!obj.is_pinned(), "the outer unpin clears it");
        }
        // Unpinned, a share marks the object and hands back its word.
        let bodies = vec![NativeBody { name: "g".into(), entry: Some(Label(0)), mir: {
            let mut g = ident("g", MirTy::HeapRef, MirTy::HeapRef);
            g.types.push(MirTy::HeapRef);
            g.blocks[0].insts.push(MirInst::Cow { dest: ValueId(1), src: ValueId(0), unique: false, field: None });
            g.blocks[0].term = Some(Terminator::Return { lo: Some(ValueId(1)), hi: None, more: Vec::new() });
            g
        } }];
        let p = jit(&bodies, &HashMap::from([("g".to_string(), 0u32)])).expect("jit");
        assert_eq!(run(&p.entries[0], &[obj.addr()]), (0, obj.addr()));
        assert!(obj.is_shared());
    }

    #[test]
    fn a_forward_string_byte_scan_passes_the_verifier() {
        // `scan(s, from, bound, c)`: the first `s[i] == c` in `from..bound`.
        // Debug builds verify the CLIF, and a refused body leaves the
        // program's native code incomplete.
        use compiler::native_mir::{LaneExpr, LaneVal, MirCmpOp, MirConst};
        let mut f = MirFunc::new("scan");
        f.blocks.clear();
        f.types = vec![MirTy::HeapRef, MirTy::I64, MirTy::I64, MirTy::I64, MirTy::I64, MirTy::I64];
        f.params = vec![ValueId(0), ValueId(1), ValueId(2), ValueId(3)];
        f.ret_ty = Some(MirTy::I64);
        let mut b = MirBlock::new(BlockId(0));
        b.insts.push(MirInst::Const { dest: ValueId(4), c: MirConst::I64(0) });
        b.insts.push(MirInst::ByteScan {
            dest: ValueId(5),
            from: ValueId(1),
            bound: ValueId(2),
            inclusive: false,
            backward: false,
            streams: vec![(ValueId(0), ValueId(4))],
            scalars: vec![ValueId(3)],
            expr: LaneExpr::Cmp { op: MirCmpOp::Eq, lhs: LaneVal::Stream(0), rhs: LaneVal::Scalar(0) },
        });
        b.term = Some(Terminator::Return { lo: Some(ValueId(5)), hi: None, more: Vec::new() });
        f.blocks.push(b);
        let bodies = vec![NativeBody { name: "scan".into(), entry: Some(Label(0)), mir: f }];
        let pcs = HashMap::from([("scan".to_string(), 0u32)]);
        let p = jit(&bodies, &pcs).expect("jit");
        assert!(p.refused.is_empty(), "refused: {:?}", p.refused);
    }

    #[test]
    fn a_callee_that_fails_to_lower_refuses_its_callers() {
        // `g` passes `plan` but has no lowering (`!` of a float); `f` calls it.
        let mut g = MirFunc::new("g");
        g.blocks.clear();
        g.types = vec![MirTy::F64, MirTy::Bool];
        g.params = vec![ValueId(0)];
        g.ret_ty = Some(MirTy::Bool);
        let mut b = MirBlock::new(BlockId(0));
        b.insts.push(MirInst::Unary { dest: ValueId(1), op: MirUnaryOp::Not, src: ValueId(0) });
        b.term = Some(Terminator::Return { lo: Some(ValueId(1)), hi: None, more: Vec::new() });
        g.blocks.push(b);
        let mut f = MirFunc::new("f");
        f.blocks.clear();
        f.types = vec![MirTy::F64, MirTy::Bool];
        f.params = vec![ValueId(0)];
        f.ret_ty = Some(MirTy::Bool);
        let mut b = MirBlock::new(BlockId(0));
        b.insts.push(MirInst::Call { dest: ValueId(1), dest_hi: None, dest_more: Vec::new(), target: Label(1), args: vec![ValueId(0)] });
        b.term = Some(Terminator::Return { lo: Some(ValueId(1)), hi: None, more: Vec::new() });
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

    /// Bodies that return three and four words to direct calls
    /// (`MirFunc::ret_more_tys`) and callers that fold the words in order:
    /// `sum3` (label 0) calls `wide3` (1); `sum4` (4) calls `pass4` (3),
    /// which tail-calls `wide4` (2). Each `wide` returns `x`, `x + 10`,
    /// `x * 10` and (`wide4`) `x - 10`.
    fn wide_bodies() -> Vec<NativeBody> {
        use compiler::native_mir::{MirBinOp, MirConst, MirTy};
        let v = ValueId;
        let i64s = |n: usize| vec![MirTy::I64; n];
        let body = |name: &str, label: u32, types: usize, more: usize, insts: Vec<MirInst>, ret: Vec<u32>| {
            let mut f = MirFunc::new(name);
            f.blocks.clear();
            f.types = i64s(types);
            f.params = vec![v(0)];
            f.ret_ty = Some(MirTy::I64);
            // (`ret_layout` stays `Word`: the engines read the word types.)
            if ret.len() > 1 {
                f.ret_hi_ty = Some(MirTy::I64);
            }
            f.ret_more_tys = i64s(more);
            let mut b = MirBlock::new(BlockId(0));
            b.insts = insts;
            b.term = Some(Terminator::Return { lo: Some(v(ret[0])), hi: ret.get(1).map(|&r| v(r)), more: ret[2.min(ret.len())..].iter().map(|&r| v(r)).collect() });
            f.blocks.push(b);
            NativeBody { name: name.into(), entry: Some(Label(label)), mir: f }
        };
        let bin = |dest: u32, op: MirBinOp, lhs: u32, rhs: u32| MirInst::Bin { dest: v(dest), op, ty: MirTy::I64, lhs: v(lhs), rhs: v(rhs) };
        let konst = |dest: u32, k: i64| MirInst::Const { dest: v(dest), c: MirConst::I64(k) };
        let call = |dests: &[u32], target: u32| MirInst::Call { dest: v(dests[0]), dest_hi: Some(v(dests[1])), dest_more: dests[2..].iter().map(|&d| v(d)).collect(), target: Label(target), args: vec![v(0)] };
        // `((w1 * 1000 + w2) * 1000 + w3) ...`, from value `at` on.
        let fold = |words: &[u32], at: u32| {
            let mut insts = vec![konst(at, 1000)];
            let (mut acc, mut next) = (words[0], at + 1);
            for &w in &words[1..] {
                insts.push(bin(next, MirBinOp::Mul, acc, at));
                insts.push(bin(next + 1, MirBinOp::Add, next, w));
                acc = next + 1;
                next += 2;
            }
            (insts, acc, next as usize)
        };
        let wide = |name: &str, label: u32, words: usize| {
            let mut insts = vec![konst(1, 10), bin(2, MirBinOp::Add, 0, 1), bin(3, MirBinOp::Mul, 0, 1)];
            if words == 4 {
                insts.push(bin(4, MirBinOp::Sub, 0, 1));
            }
            body(name, label, words + 1, words - 2, insts, (0..words as u32).map(|w| if w == 0 { 0 } else { w + 1 }).collect())
        };
        let sum = |name: &str, label: u32, target: u32, words: usize| {
            let dests: Vec<u32> = (1..=words as u32).collect();
            let (mut insts, out, types) = fold(&dests, words as u32 + 1);
            insts.insert(0, call(&dests, target));
            body(name, label, types, 0, insts, vec![out])
        };
        let mut pass = body("pass4", 3, 5, 2, vec![call(&[1, 2, 3, 4], 2)], vec![1, 2, 3, 4]);
        pass.mir.tail_targets.insert(Label(2));
        vec![sum("sum3", 0, 1, 3), wide("wide3", 1, 3), wide("wide4", 2, 4), pass, sum("sum4", 4, 3, 4)]
    }

    /// What `sum3` / `sum4` return for `x`: the words folded in order.
    fn wide_sums(x: i64) -> (u64, u64) {
        let k = 1000i64;
        let s3 = (x * k + (x + 10)) * k + x * 10;
        let s4 = ((x * k + (x + 10)) * k + x * 10) * k + (x - 10);
        (s3 as u64, s4 as u64)
    }

    /// A direct call reads three and four result words, a tail call passes
    /// four on, and the runtime's entry of a four-word body panics rather
    /// than hand back two of them.
    #[test]
    fn a_direct_call_reads_three_and_four_words() {
        let pcs = HashMap::from([("sum3".to_string(), 0u32), ("sum4".to_string(), 1u32), ("wide4".to_string(), 2u32)]);
        let p = jit(&wide_bodies(), &pcs).expect("jit");
        assert!(p.refused.is_empty(), "refused: {:?}", p.refused);
        let entry = |pc: u32| p.entries.iter().find(|e| e.pc == pc).expect("entry");
        for x in [7i64, -3, 1 << 20] {
            let (s3, s4) = wide_sums(x);
            assert_eq!(run(entry(0), &[x as u64]), (0, s3), "sum3({x})");
            assert_eq!(run(entry(1), &[x as u64]), (0, s4), "sum4({x})");
        }
        assert_eq!(entry(2).ret_words, 2);
        let marker = 0u8;
        let mut ctx = NativeCtx::new((std::ptr::addr_of!(marker) as u64).saturating_sub(NATIVE_STACK_BUDGET));
        let mut trace = Vec::new();
        ctx.trace = &mut trace;
        // SAFETY: compiled for one argument word.
        unsafe { (entry(2).entry)(&mut ctx, [7u64].as_ptr()) };
        assert_ne!(ctx.bail, 0, "an entry of a four-word body panics");
    }
}
