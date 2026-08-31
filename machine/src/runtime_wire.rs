//! Glue to attach a compiled program to a VM: standard HostInvoke table,
//! dload/FFI paths, env grants, C struct layouts, thread program.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{Byte, ProgramDebug};

use crate::env;
use crate::ffi::DloadGate;
use crate::memory::CStructLayout;
use crate::thread::ThreadProgram;
use crate::{Machine, set_allow_attach};

/// Inputs the compiler can supply without depending on `machine`.
pub struct VmHostSpec<'a> {
    pub entry_path: Option<&'a Path>,
    pub project_root: &'a Path,
    pub ffi_search_paths: &'a [PathBuf],
    pub ffi_allow: &'a [String],
    pub native_pins: &'a [(String, String)],
    pub trusted_stems: &'a [String],
    pub extra_dload_stems: &'a [String],
    pub extra_dload_grants: &'a [(String, PathBuf)],
    pub allow_exec: bool,
    pub allow_exit: bool,
    pub allow_ffi_exec: bool,
    pub allow_attach: bool,
    pub c_structs: &'a [common::CStructLayout],
}

/// Standard natives, dload gate, env grants, FFI search, C layouts.
pub fn wire_vm_host<const N: usize>(vm: &mut Machine<N>, spec: &VmHostSpec<'_>) {
    crate::wire_standard_host_natives(vm);

    let base_dir = spec.entry_path.and_then(|p| p.parent()).map(PathBuf::from);
    let search: Vec<PathBuf> = spec
        .ffi_search_paths
        .iter()
        .map(|p| spec.project_root.join(p))
        .collect();
    vm.set_ffi_paths(base_dir, search);

    let mut gate = DloadGate::from_consumer_trusted(
        spec.ffi_allow,
        spec.native_pins,
        spec.trusted_stems.iter().map(String::as_str),
    );
    for stem in spec.extra_dload_stems {
        gate.grant_stem(stem);
    }
    for (stem, path) in spec.extra_dload_grants {
        let _ = gate.grant_file(stem, path);
    }
    vm.set_dload_gate(gate);

    env::set_allow_exec(spec.allow_exec);
    env::set_allow_exit(spec.allow_exit);
    env::set_allow_ffi_exec(spec.allow_ffi_exec);
    set_allow_attach(spec.allow_attach);

    for layout in spec.c_structs {
        vm.register_struct_layout(CStructLayout::from_archive(layout));
    }
}

/// Shared bytecode for `thread::spawn` workers.
pub fn wire_thread_program<const N: usize>(
    machine: &mut Machine<N>,
    bytecode: &[Byte],
    constants: &[u64],
    strings: &[String],
    static_slot_count: u32,
    debug: ProgramDebug,
    operand_stack_slots: u32,
) {
    machine.set_thread_program(Arc::new(ThreadProgram {
        code: Arc::from(bytecode.to_vec()),
        constants: Arc::from(constants.to_vec()),
        strings: Arc::from(strings.to_vec()),
        static_slot_count,
        debug,
        operand_stack_slots,
    }));
}
