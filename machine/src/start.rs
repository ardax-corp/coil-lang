//! A program's start with no interpreter: a [`Runtime`] built for the
//! program and the native driver over it, no `Machine`. An AOT executable
//! starts here when its native code covers every body
//! (`docs/internals/native.md`, "The native driver").

use std::path::PathBuf;
use std::sync::Arc;

use common::Value;

use crate::driver::{Driver, DriverProgram};
use crate::runtime::Runtime;
use crate::thread::ThreadProgram;

/// What a native start needs besides the program: where its FFI libraries
/// are found and the C struct layouts it declares.
#[derive(Default)]
pub struct NativeStart {
    pub base_dir: Option<PathBuf>,
    pub ffi_search_paths: Vec<PathBuf>,
    pub dload_gate: Option<crate::ffi::DloadGate>,
    pub struct_layouts: Vec<crate::CStructLayout>,
}

/// Run `program` on the native driver with the native code installed for
/// the next run ([`crate::native::install_for_next_run`],
/// [`crate::native::install_aot`]): `Some(panicked)`. `None` when that code
/// does not cover every body or the driver is off: the code stays
/// installed and the caller runs the program on the VM.
pub fn run_natively(program: Arc<ThreadProgram>, start: NativeStart) -> Option<bool> {
    if !crate::driver::enabled() || !crate::native::pending_is_complete() {
        return None;
    }
    let native = crate::native::take_pending()?.into_code(program.code.len());
    if native.lookup(0).is_none() && !Driver::can_start(&program.code, &native, 0) {
        crate::native::install_shared_for_next_run(native);
        return None;
    }
    let mut rt = runtime_for(&program, start);
    rt.native = Some(Box::new(crate::native::NativeTable::new(Arc::clone(&native))));
    let driver_program = DriverProgram {
        code: Arc::clone(&program.code),
        constants: Arc::clone(&program.constants),
        native,
        debug: program.debug.clone(),
        base_dir: rt.ffi.base_dir.clone(),
        thread_program: Some(Arc::clone(&program)),
        shared_print: None,
    };
    let mut driver = Driver::new(&mut rt, driver_program);
    let guard = crate::thread::HostStateGuard::enter_driver(&mut driver);
    driver.run_from(0);
    driver.run_remaining_finalizers();
    drop(guard);
    let panicked = driver.panicked();
    drop(driver);
    if std::env::var_os("COIL_NATIVE_STATS").is_some() {
        eprintln!("native: the driver ran the program (no interpreter)");
        if let Some(t) = rt.native.as_deref() {
            crate::native::print_stats(t);
        }
    }
    finish(&mut rt);
    Some(panicked)
}

/// The runtime `program` runs over: the standard host natives, its FFI
/// paths and layouts, its statics, strings and word kinds.
fn runtime_for(program: &ThreadProgram, start: NativeStart) -> Runtime {
    let mut rt = Runtime::new();
    for native in crate::host_natives::build_standard_host_natives(|_, _| {}) {
        rt.natives.register(native);
    }
    rt.ffi.base_dir = start.base_dir;
    rt.ffi.search_paths = start.ffi_search_paths;
    if let Some(gate) = start.dload_gate {
        rt.ffi.dload_gate = gate;
    }
    Arc::make_mut(&mut rt.ffi.struct_layouts).extend(start.struct_layouts);
    rt.heap.set_class_word_kinds(Arc::clone(&program.class_word_kinds));
    crate::collect::STATIC_KINDS.with_borrow_mut(|k| *k = Arc::clone(&program.static_word_kinds));
    rt.statics = vec![Value::default(); program.static_slot_count as usize];
    rt.program_strings = Arc::clone(&program.strings);
    rt.program_string_cache = vec![Value::default(); program.strings.len()];
    rt
}

/// After `main`: join the threads it left running and stop the reactor's
/// workers (as the VM's run does).
fn finish(rt: &mut Runtime) {
    if crate::reactor::NO_THREADS {
        while rt.reactor.help_local_once() {}
    }
    crate::thread::join_undetached_threads(&rt.live_threads);
    if rt.reactor.inflight() == 0 {
        rt.reactor.shutdown();
    }
}
