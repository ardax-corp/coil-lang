//! Run a compiled [`Pipeline`] without `compiler` depending on `machine`.
//!
//! Shared by `coil` (default run) and `coil-test` (harness). Keeps the host wiring in one
//! place so the two binaries cannot drift.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::Byte;
use compiler::Pipeline;
use machine::{CStructLayout, DloadGate, NativeStart, ThreadProgram};

/// Fail-closed dload integrity for binaries that run a compiled Pipeline (compiler `machine` dep is optional).
///
/// Compile-time `--allow-dload` is not re-applied. Pins, trusted, and host
/// extra grants remain locators / integrity.
pub fn pipeline_dload_gate(pipeline: &Pipeline) -> DloadGate {
    let pins = pipeline.dload_native_pins();
    let trusted = pipeline.dload_trusted_stems();
    let mut gate = DloadGate::from_consumer_trusted(&pins, &trusted);
    for stem in pipeline.extra_dload_stems() {
        gate.grant_stem(stem);
    }
    for (stem, path) in pipeline.extra_dload_grants() {
        let _ = gate.grant_file(stem, path);
    }
    gate
}

/// How `pipeline`'s program starts: its FFI paths, dload gate and C layouts.
pub fn native_start(pipeline: &Pipeline, structs: &[common::CStructLayout], entry: Option<&Path>) -> NativeStart {
    NativeStart {
        base_dir: entry.and_then(Path::parent).map(PathBuf::from),
        ffi_search_paths: pipeline.ffi_search_path_bufs().iter().map(|p| pipeline.project_root().join(p)).collect(),
        dload_gate: Some(pipeline_dload_gate(pipeline)),
        struct_layouts: structs.iter().map(CStructLayout::from_archive).collect(),
        shared_print: None,
    }
}

/// What a pipeline hands the runtime after a compile.
pub struct CompiledProgram<'a> {
    pub bytecode: &'a [Byte],
    pub constants: &'a [u64],
    pub strings: &'a [String],
    pub static_slots: u32,
    pub debug: common::ProgramDebug,
    pub operand_stack_slots: u32,
}

/// `program` as jobs and the driver take it, with `pipeline`'s word maps.
pub fn thread_program(pipeline: &Pipeline, program: CompiledProgram<'_>) -> ThreadProgram {
    ThreadProgram {
        code: Arc::new(program.bytecode.to_vec()),
        constants: Arc::new(program.constants.to_vec()),
        strings: Arc::new(program.strings.to_vec()),
        static_slot_count: program.static_slots,
        debug: program.debug,
        operand_stack_slots: program.operand_stack_slots,
        stack_maps: Vec::new(),
        precise_frames: Arc::new(pipeline.precise_frames().to_vec()),
        class_word_kinds: machine::class_kind_table(&pipeline.class_word_kinds()),
        static_word_kinds: Arc::new(pipeline.static_word_kinds()),
    }
}

/// Canonical entry path for FFI relative lookups (falls back to the path as given).
pub fn ffi_entry_path(entry: &Path) -> PathBuf {
    std::fs::canonicalize(entry).unwrap_or_else(|_| entry.to_path_buf())
}

/// Bind the default `src` root under cwd plus any extra `--root` directories.
pub fn bind_cli_roots(pipeline: &mut Pipeline, extra: Vec<PathBuf>) {
    let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    pipeline.bind_project_roots_with_default(dir, extra);
}

/// Inputs for [`execute_pipeline`]: an in-memory compile, never a `.ccoil` round trip.
pub struct ExecutePipelineArgs<'a> {
    pub pipeline: &'a Pipeline,
    pub bytecode: &'a [Byte],
    pub constants: &'a [u64],
    pub strings: &'a [String],
    pub static_slots: u32,
    pub debug: common::ProgramDebug,
    pub entry: Option<&'a Path>,
    pub operand_stack_slots: u32,
}

/// Run a compiled program from `main`. Returns `true` when a language-level `panic` aborted.
/// Uncaught `raise` from `main` is a `Result.Err` return and is not an abort (Q5).
pub fn execute_pipeline(args: ExecutePipelineArgs<'_>) -> bool {
    let ExecutePipelineArgs { pipeline, bytecode, constants, strings, static_slots, debug, entry, operand_stack_slots } = args;
    let entry = entry.map(ffi_entry_path);
    let program = thread_program(pipeline, CompiledProgram { bytecode, constants, strings, static_slots, debug, operand_stack_slots });
    run(program, native_start(pipeline, &pipeline.archived_struct_layouts(), entry.as_deref()))
}

/// A program read back from the build cache: what [`execute_pipeline`]
/// takes from the pipeline after a compile. The run-time grants (dload pins,
/// FFI search paths) still come from the pipeline's flags.
pub struct CachedProgram<'a> {
    pub bytecode: &'a [Byte],
    pub constants: &'a [u64],
    pub strings: &'a [String],
    pub static_slots: u32,
    pub debug: common::ProgramDebug,
    pub operand_stack_slots: u32,
    pub struct_layouts: &'a [common::CStructLayout],
    pub precise_frames: Vec<common::PreciseFrameMap>,
    pub class_word_kinds: Vec<common::ClassWordKinds>,
    pub static_word_kinds: Vec<u8>,
}

/// Run a cached program from `main` as [`execute_pipeline`] runs a fresh
/// compile. Returns `true` when a language-level `panic` aborted.
pub fn execute_cached(pipeline: &Pipeline, program: CachedProgram<'_>, entry: Option<&Path>) -> bool {
    let entry = entry.map(ffi_entry_path);
    let start = native_start(pipeline, program.struct_layouts, entry.as_deref());
    run(
        ThreadProgram {
            code: Arc::new(program.bytecode.to_vec()),
            constants: Arc::new(program.constants.to_vec()),
            strings: Arc::new(program.strings.to_vec()),
            static_slot_count: program.static_slots,
            debug: program.debug,
            operand_stack_slots: program.operand_stack_slots,
            stack_maps: Vec::new(),
            precise_frames: Arc::new(program.precise_frames),
            class_word_kinds: machine::class_kind_table(&program.class_word_kinds),
            static_word_kinds: Arc::new(program.static_word_kinds),
        },
        start,
    )
}

fn run(program: ThreadProgram, start: NativeStart) -> bool {
    machine::run_natively(Arc::new(program), start).unwrap_or_else(|| {
        eprintln!("coil: the code installed does not cover this program");
        true
    })
}
