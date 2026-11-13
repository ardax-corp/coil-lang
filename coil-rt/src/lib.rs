//! Runtime library of an AOT executable (`coil package`, see
//! `docs/internals/native.md`, "AOT"): `cc` links it with the object
//! `coil_native::aot` made, which defines the compiled bodies, their tables
//! and the program's package. `main` installs the linked code, then runs the
//! package on the native driver. `coil package` links one only when native
//! code covers every body.

// The libtest build leaves out `main`, so its imports and tables go unused.
#![cfg_attr(test, allow(dead_code, unused_imports))]

use std::process::exit;

unsafe extern "C" {
    static coil_aot_entries: machine::native::AotEntry;
    static coil_aot_entry_count: u64;
    static coil_aot_sites: u32;
    static coil_aot_site_count: u64;
    static coil_aot_refused_count: u64;
    static coil_aot_layout: u64;
    static coil_aot_stack_maps: u8;
    static coil_aot_stack_map_count: u64;
    static coil_static_syms: machine::StaticSymbol;
    static coil_static_sym_count: u64;
    static coil_aot_host_groups: machine::AotHostGroup;
    static coil_aot_host_group_count: u64;
    static coil_aot_package: u8;
    static coil_aot_package_len: u64;
}

/// The executable's entry (the C runtime calls it).
/// Left out of the libtest build, whose harness brings its own `main`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    // SAFETY: the linked object defines the tables as `aot` wrote them.
    unsafe {
        let installed = machine::native::install_aot(
            &raw const coil_aot_entries,
            coil_aot_entry_count as usize,
            &raw const coil_aot_sites,
            coil_aot_site_count as usize,
            coil_aot_refused_count == 0,
            coil_aot_layout,
        );
        if let Err(e) = installed {
            eprintln!("coil: {e}");
            exit(1);
        }
        machine::native::install_aot_stack_maps(&raw const coil_aot_stack_maps, coil_aot_stack_map_count as usize);
        machine::install_static_symbols(&raw const coil_static_syms, coil_static_sym_count as usize);
        machine::native::install_aot_host_groups(&raw const coil_aot_host_groups, coil_aot_host_group_count as usize);
    }
    // SAFETY: the linked object defines the package as `aot` wrote it.
    let package = unsafe { std::slice::from_raw_parts(&raw const coil_aot_package, coil_aot_package_len as usize) };
    match coil_cli::run_linked_package(package) {
        // A language `panic` exits 1, as `coil-embed`.
        Some(panicked) => exit(i32::from(panicked)),
        None => {
            eprintln!("coil: this executable's package is corrupt");
            exit(1);
        }
    }
}
