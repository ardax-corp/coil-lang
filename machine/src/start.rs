//! A program's start with no interpreter: a [`Runtime`] built for the
//! program and the native driver over it. An AOT executable
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
    /// Where `print` goes instead of stdout (a debugger's client).
    pub shared_print: Option<Arc<std::sync::Mutex<Vec<u8>>>>,
    /// The host natives, by id: [`standard_host_natives`], or an AOT
    /// executable's own ([`crate::native::aot_host_natives`]). The start
    /// names no default, so a link that never starts the standard table
    /// drops the natives its program does not call.
    pub host_natives: Vec<Arc<dyn crate::ffi::NativeFn>>,
}

/// Every standard host native, by id (built once per process).
pub fn standard_host_natives() -> Vec<Arc<dyn crate::ffi::NativeFn>> {
    static TABLE: std::sync::OnceLock<Vec<Arc<dyn crate::ffi::NativeFn>>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| crate::host_natives::build_standard_host_natives(|_, _| {})).clone()
}

/// Run `program` on the native driver with the native code installed for
/// the next run ([`crate::native::install_for_next_run`],
/// [`crate::native::install_aot`]): `Some(panicked)`. `None` when that code
/// does not cover every body: nothing ran.
pub fn run_natively(program: Arc<ThreadProgram>, start: NativeStart) -> Option<bool> {
    if !crate::native::pending_is_complete() {
        return None;
    }
    let native = crate::native::take_pending()?.into_code();
    if !Driver::covers(&program.startup, program.main, &native) {
        crate::native::install_shared_for_next_run(native);
        return None;
    }
    let shared_print = start.shared_print.clone();
    if shared_print.is_some() {
        crate::io::set_shared_print_redirect(shared_print.clone());
    }
    let mut rt = runtime_for(&program, start);
    rt.native = Some(Box::new(crate::native::NativeTable::new(Arc::clone(&native))));
    let driver_program = DriverProgram {
        startup: Arc::clone(&program.startup),
        main: program.main,
        constants: Arc::clone(&program.constants),
        native,
        debug: Arc::new(program.debug.clone()),
        thread_program: Some(Arc::clone(&program)),
        shared_print,
    };
    if std::env::var_os("COIL_COW_REPORT").is_some() {
        rt.heap.get_mut().report_cow();
    }
    let mut driver = Driver::new(&mut rt, driver_program);
    let guard = crate::thread::HostStateGuard::enter_driver(&mut driver);
    driver.run_program();
    driver.run_remaining_finalizers();
    driver.print_cow_report();
    drop(guard);
    let panicked = driver.panicked();
    drop(driver);
    if std::env::var_os("COIL_NATIVE_STATS").is_some() {
        eprintln!("native: the driver ran the program");
        if let Some(t) = rt.native.as_deref() {
            crate::native::print_stats(t);
        }
    }
    finish(&mut rt);
    Some(panicked)
}

/// What a job needs to run `program`, for a host that only submits jobs (the
/// test harness).
pub fn spawn_context(
    program: Arc<ThreadProgram>,
    start: NativeStart,
    reactor: Arc<crate::reactor::Reactor>,
    native: Option<Arc<crate::native::NativeCode>>,
) -> Option<crate::thread::ThreadSpawnContext> {
    let mut rt = runtime_for(&program, start);
    rt.reactor = reactor;
    rt.native = native.map(|n| Box::new(crate::native::NativeTable::new(n)));
    rt.thread_spawn_context(program, None)
}

/// The runtime `program` runs over: the standard host natives, its FFI
/// paths and layouts, its statics, strings and word kinds.
fn runtime_for(program: &ThreadProgram, start: NativeStart) -> Runtime {
    let mut rt = Runtime::new();
    for native in start.host_natives {
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
/// workers (as the runtime's run does).
fn finish(rt: &mut Runtime) {
    if crate::reactor::NO_THREADS {
        while rt.reactor.help_local_once() {}
    }
    crate::thread::join_undetached_threads(&rt.live_threads);
    if rt.reactor.inflight() == 0 {
        rt.reactor.shutdown();
    }
}
