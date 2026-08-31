//! Attach a compiled Pipeline to a Machine without compiler depending on machine.

use std::path::Path;

use common::Byte;
use compiler::Pipeline;
use machine::{Machine, VmHostSpec, wire_thread_program, wire_vm_host};

pub fn wire_pipeline_vm<const N: usize>(
    pipeline: &Pipeline,
    machine: &mut Machine<N>,
    entry: Option<&Path>,
) {
    let pins = pipeline.dload_native_pins();
    let trusted = pipeline.dload_trusted_stems();
    let structs = pipeline.archived_struct_layouts();
    let m = pipeline.manifest();
    wire_vm_host(
        machine,
        &VmHostSpec {
            entry_path: entry,
            project_root: pipeline.project_root(),
            ffi_search_paths: &m.ffi_search_paths,
            ffi_allow: &m.ffi_allow,
            native_pins: &pins,
            trusted_stems: &trusted,
            extra_dload_stems: pipeline.extra_dload_stems(),
            extra_dload_grants: pipeline.extra_dload_grants(),
            allow_exec: m.allow_exec,
            allow_exit: m.allow_exit,
            allow_ffi_exec: m.allow_ffi_exec,
            allow_attach: m.allow_attach,
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
