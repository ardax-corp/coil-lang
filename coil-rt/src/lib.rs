//! Runtime library of an AOT executable (`coil package`, see
//! `docs/internals/native.md`, "AOT"): `cc` links it with the object
//! `coil_native::aot` made, which defines the compiled bodies and their
//! tables, and `coil package` appends the program's archive as it does to
//! a `coil-embed` runner. `main` installs the linked code, then runs the
//! archive as `coil-embed` does: native code where the object has it, the
//! VM for everything else.

use std::process::exit;

unsafe extern "C" {
    static coil_aot_entries: machine::native::AotEntry;
    static coil_aot_entry_count: u64;
    static coil_aot_sites: u32;
    static coil_aot_site_count: u64;
}

/// The executable's entry (the C runtime calls it).
#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    // SAFETY: the linked object defines the tables as `aot` wrote them.
    unsafe {
        machine::native::install_aot(
            &raw const coil_aot_entries,
            coil_aot_entry_count as usize,
            &raw const coil_aot_sites,
            coil_aot_site_count as usize,
        );
    }
    match coil_cli::try_run_embedded() {
        // A language `panic` exits 1, as `coil-embed`.
        Some(panicked) => exit(i32::from(panicked)),
        None => {
            eprintln!("coil: this AOT executable carries no program (build it with `coil package`)");
            exit(1);
        }
    }
}
