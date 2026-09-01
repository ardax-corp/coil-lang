//! Attach a compiled Pipeline to a Machine without compiler depending on machine.

use std::path::Path;

use common::Byte;
use compiler::Pipeline;
use machine::{DloadGate, Machine, VmHostSpec, wire_thread_program, wire_vm_host};

/// Fail-closed dload gate for the coil binary (compiler `machine` dep is optional).
pub fn pipeline_dload_gate(pipeline: &Pipeline) -> DloadGate {
    let pins = pipeline.dload_native_pins();
    let trusted = pipeline.dload_trusted_stems();
    let grants = pipeline.host_grants();
    let mut gate = DloadGate::from_consumer_trusted(&grants.allow_dload, &pins, &trusted);
    for stem in pipeline.extra_dload_stems() {
        gate.grant_stem(stem);
    }
    for (stem, path) in pipeline.extra_dload_grants() {
        let _ = gate.grant_file(stem, path);
    }
    gate.set_allow_attach(grants.allow_attach);
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
    let grants = pipeline.host_grants();
    let search = pipeline.ffi_search_path_bufs();
    wire_vm_host(
        machine,
        &VmHostSpec {
            entry_path: entry,
            project_root: pipeline.project_root(),
            ffi_search_paths: &search,
            ffi_allow: &grants.allow_dload,
            native_pins: &pins,
            trusted_stems: &trusted,
            extra_dload_stems: pipeline.extra_dload_stems(),
            extra_dload_grants: pipeline.extra_dload_grants(),
            allow_exec: grants.allow_exec,
            allow_exit: grants.allow_exit,
            allow_ffi_exec: grants.allow_ffi_exec,
            allow_attach: grants.allow_attach,
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
    wire_thread_program(
        machine,
        bytecode,
        constants,
        strings,
        pipeline.static_slot_count(),
        pipeline.program_debug(),
        pipeline.operand_stack_slots(),
    );
}
