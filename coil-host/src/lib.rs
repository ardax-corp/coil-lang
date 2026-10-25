//! Attach a compiled [`Pipeline`] to a [`Machine`] without `compiler` depending on `machine`.
//!
//! Shared by `coil` (default run) and `coil-test` (harness). Keeps the host wiring in one
//! place so the two binaries cannot drift.

use std::path::{Path, PathBuf};

use common::Byte;
use compiler::Pipeline;
use machine::{
    DloadGate, Machine, VmHostSpec, WireThreadProgramWithMapsArgs, wire_thread_program_with_maps,
    wire_vm_host,
};

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

pub fn wire_pipeline_vm<const N: usize>(
    pipeline: &Pipeline,
    machine: &mut Machine<N>,
    entry: Option<&Path>,
) {
    let pins = pipeline.dload_native_pins();
    let trusted = pipeline.dload_trusted_stems();
    let structs = pipeline.archived_struct_layouts();
    let search = pipeline.ffi_search_path_bufs();
    wire_vm_host(
        machine,
        &VmHostSpec {
            entry_path: entry,
            project_root: pipeline.project_root(),
            ffi_search_paths: &search,
            native_pins: &pins,
            trusted_stems: &trusted,
            extra_dload_stems: pipeline.extra_dload_stems(),
            extra_dload_grants: pipeline.extra_dload_grants(),
            c_structs: &structs,
        },
    );
}

pub fn wire_pipeline_threads<const N: usize>(
    pipeline: &Pipeline,
    machine: &mut Machine<N>,
    bytecode: &[Byte],
    constants: &[u64],
    strings: &[String],
) {
    wire_thread_program_with_maps(WireThreadProgramWithMapsArgs {
        machine,
        bytecode,
        constants,
        strings,
        static_slot_count: pipeline.static_slot_count(),
        debug: pipeline.program_debug(),
        operand_stack_slots: pipeline.operand_stack_slots(),
        stack_maps: Vec::new(),
        precise_frames: pipeline.precise_frames().to_vec(),
        class_word_kinds: pipeline.class_word_kinds(),
        static_word_kinds: pipeline.static_word_kinds(),
    });
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

/// Inputs for [`execute_pipeline`]: an in-memory compile, never a `.hyc` round trip.
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
    let ExecutePipelineArgs {
        pipeline,
        bytecode,
        constants,
        strings,
        static_slots,
        debug,
        entry,
        operand_stack_slots,
    } = args;
    let operand_slots =
        operand_stack_slots.max(machine::DEFAULT_OPERAND_STACK_SLOTS as u32) as usize;
    let entry = entry.map(ffi_entry_path);
    let mut machine = Machine::<256>::with_operand_capacity(operand_slots);
    wire_pipeline_vm(pipeline, &mut machine, entry.as_deref());
    wire_pipeline_threads(pipeline, &mut machine, bytecode, constants, strings);
    machine.set_program_debug(debug);
    machine.run_raw(bytecode, constants, strings, static_slots);
    machine.panicked()
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
    let operand_slots =
        program.operand_stack_slots.max(machine::DEFAULT_OPERAND_STACK_SLOTS as u32) as usize;
    let entry = entry.map(ffi_entry_path);
    let mut machine = Machine::<256>::with_operand_capacity(operand_slots);
    let pins = pipeline.dload_native_pins();
    let trusted = pipeline.dload_trusted_stems();
    let search = pipeline.ffi_search_path_bufs();
    wire_vm_host(
        &mut machine,
        &VmHostSpec {
            entry_path: entry.as_deref(),
            project_root: pipeline.project_root(),
            ffi_search_paths: &search,
            native_pins: &pins,
            trusted_stems: &trusted,
            extra_dload_stems: pipeline.extra_dload_stems(),
            extra_dload_grants: pipeline.extra_dload_grants(),
            c_structs: program.struct_layouts,
        },
    );
    wire_thread_program_with_maps(WireThreadProgramWithMapsArgs {
        machine: &mut machine,
        bytecode: program.bytecode,
        constants: program.constants,
        strings: program.strings,
        static_slot_count: program.static_slots,
        debug: program.debug.clone(),
        operand_stack_slots: program.operand_stack_slots,
        stack_maps: Vec::new(),
        precise_frames: program.precise_frames,
        class_word_kinds: program.class_word_kinds,
        static_word_kinds: program.static_word_kinds,
    });
    machine.set_program_debug(program.debug);
    machine.run_raw(program.bytecode, program.constants, program.strings, program.static_slots);
    machine.panicked()
}
