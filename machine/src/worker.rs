//! A reactor job (a thread, an auto-par body, a harness case) whose native
//! code covers its whole program runs on the driver over a [`Runtime`] of
//! the worker's own.

use std::cell::RefCell;
use std::sync::Arc;

use common::Value;

use crate::driver::{Driver, DriverProgram};
use crate::reactor::{Job, JobStatics};
use crate::runtime::Runtime;
use crate::thread::{HostStateGuard, PortableValue, SharedPrintWriter, ThreadErrorTag};

thread_local! {
    /// Idle job runtimes of this thread: a joining job help-steals another
    /// onto its own thread, so a nest takes several at once.
    static RUNTIMES: RefCell<Vec<Runtime>> = const { RefCell::new(Vec::new()) };
}

/// Whether `job`'s native code covers every body and its entry, so
/// [`run_native_job`] can run it.
pub(crate) fn runs_natively(job: &Job) -> bool {
    let Some(native) = job.native.as_ref().filter(|n| n.is_complete()) else {
        return false;
    };
    let startup = job.test.is_none() || Driver::covers(&job.program.startup, None, native);
    native.lookup(job.entry as usize).is_some() && startup
}

/// Run `job` (one [`runs_natively`] accepts) on the driver and store its
/// result.
pub(crate) fn run_native_job(job: Job) {
    let mut rt = RUNTIMES.with(|p| p.borrow_mut().pop()).unwrap_or_else(Runtime::new);
    run_on(&mut rt, job);
    RUNTIMES.with(|p| {
        let mut pool = p.borrow_mut();
        pool.push(rt);
        // Keep one idle runtime: peak RSS is not every nest depth's.
        pool.truncate(1);
    });
}

fn run_on(rt: &mut Runtime, job: Job) {
    let Job {
        entry,
        args,
        state,
        program,
        natives,
        shared_print,
        shared_stdout,
        shared_stderr,
        live_threads,
        reactor,
        io_reactor,
        ffi_base_dir,
        ffi_search_paths,
        dload_gate,
        epoch,
        struct_layouts,
        test,
        statics,
        native,
        speculative,
    } = job;
    // Cancelled before it started: nothing to run (#2442).
    if state.cancelled() {
        state.store_result_with_panic(Err(ThreadErrorTag::JoinFailed), None);
        reactor.job_done();
        return;
    }
    let job_scope = crate::thread::JobScope::enter(speculative.then(|| Arc::clone(&state)));
    // The print redirects of a job this thread runs while helping a join
    // are put back after it (as the runtime's job run does).
    let redirected = shared_print.is_some();
    let prev_output = redirected.then(|| crate::io::set_output_redirect(None)).flatten();
    let prev_shared_print = redirected.then(|| crate::io::set_shared_print_redirect(None)).flatten();
    let prev_shared_stdout = redirected.then(|| crate::io::set_shared_stdout_redirect(None)).flatten();
    let prev_shared_stderr = redirected.then(|| crate::io::set_shared_stderr_redirect(None)).flatten();
    let prev_captures = crate::io::take_captures();
    let shared = epoch.is_some();
    if let Some(e) = &epoch {
        rt.heap.bind(e.heap_ptr());
        rt.shared_epoch = Some(Arc::clone(e));
    }
    // A list of its own: a job of an earlier run must not find it.
    rt.speculative_out = crate::thread::SpeculativeOut::default();
    let outer_exit = crate::env::take_exit_request();
    let outer_static_kinds = crate::collect::swap_static_word_kinds(Arc::clone(&program.static_word_kinds));
    let mut panic_message = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.natives = natives.clone_registry();
        rt.heap.set_class_word_kinds(Arc::clone(&program.class_word_kinds));
        rt.live_threads = Arc::clone(&live_threads);
        rt.reactor = Arc::clone(&reactor);
        rt.io_reactor = Arc::clone(&io_reactor);
        rt.worker_cap = crate::thread::WorkerCap::from_count(reactor.worker_count());
        rt.ffi.base_dir = ffi_base_dir;
        rt.ffi.search_paths = ffi_search_paths;
        rt.ffi.dload_gate = dload_gate;
        rt.ffi.struct_layouts = struct_layouts;
        rt.output = None;
        if let Some(buf) = &shared_print {
            rt.output = Some(Box::new(SharedPrintWriter(Arc::clone(buf))));
            if let Some(out) = rt.output.as_mut() {
                crate::io::set_output_redirect(Some(out.as_mut() as *mut (dyn std::io::Write + Send)));
            }
            crate::io::set_shared_print_redirect(Some(Arc::clone(buf)));
            rt.shared_stdout = shared_stdout.clone();
            crate::io::set_shared_stdout_redirect(shared_stdout.clone());
            rt.shared_stderr = shared_stderr.clone();
            crate::io::set_shared_stderr_redirect(shared_stderr.clone());
        }
        rt.program_strings = Arc::clone(&program.strings);
        rt.program_string_cache.clear();
        rt.program_string_cache.resize(program.strings.len(), Value::default());
        let native = native.expect("runs_natively checked the native code");
        rt.native = Some(Box::new(crate::native::NativeTable::new(Arc::clone(&native))));
        // A job reads the spawner's statics, as on the runtime.
        match statics {
            Some(JobStatics::Shared(words)) if shared => rt.statics = words.iter().map(|&w| Value::from(w)).collect(),
            statics => {
                rt.statics = vec![Value::default(); program.static_slot_count as usize];
                if let Some(JobStatics::Copied(slots)) = statics {
                    for (i, arg) in slots.into_iter().enumerate().take(rt.statics.len()) {
                        if let Some(arg) = arg
                            && let Ok(v) = crate::thread::spawn_arg_to_value(rt.heap.get_mut(), arg)
                        {
                            rt.statics[i] = v;
                        }
                    }
                }
            }
        }
        let driver_program = DriverProgram {
            startup: Arc::clone(&program.startup),
            main: program.main,
            constants: Arc::clone(&program.constants),
            native,
            debug: Arc::new(program.debug.clone()),
            thread_program: Some(Arc::clone(&program)),
            shared_print: shared_print.clone(),
        };
        let mut d = Driver::new(rt, driver_program);
        d.set_quiet(speculative);
        if speculative {
            // On the interpreter a cancel is seen when a slice runs out.
            d.rt.fuel = crate::thread::FUEL_SLICE;
        }
        let guard = HostStateGuard::enter_driver(&mut d);
        let write_scope = crate::memory::EpochWriteScope::enter(d.rt.heap.get(), shared);
        let out = (|| {
            if let Some(test) = &test {
                d.rt.begin_probes(test.probes);
                d.rt.set_step_budget(test.step_budget);
                let ret = crate::reactor::test_on_driver(&mut d, entry, test);
                let hits = d.rt.take_probes();
                let steps = (d.rt.steps_charged(), d.rt.step_budget_hit);
                d.rt.set_step_budget(None);
                return Ok(crate::reactor::finish_test(d.panicked(), ret, d.rt.heap.get(), test, hits, steps));
            }
            let mut child_args = Vec::with_capacity(args.len());
            for a in args {
                child_args.push(crate::thread::spawn_arg_to_value(d.rt.heap.get_mut(), a)?);
            }
            let ret = d.call(entry as usize, &child_args).map_or_else(Value::default, |(lo, _)| Value::from(lo));
            if epoch.as_ref().is_some_and(|e| e.is_aborted()) {
                return Err(ThreadErrorTag::JoinFailed);
            }
            if d.panicked() {
                panic_message = Some(d.take_panic_message());
                return Err(ThreadErrorTag::Panicked);
            }
            if shared { Ok(PortableValue::Immediate(ret.bits())) } else { crate::thread::value_to_portable(d.rt.heap.get(), ret) }
        })();
        drop(write_scope);
        drop(guard);
        out
    }));
    crate::env::restore_exit_request(outer_exit);
    crate::collect::swap_static_word_kinds(outer_static_kinds);
    crate::io::flush_open_captures();
    if redirected {
        crate::io::set_output_redirect(prev_output);
        crate::io::set_shared_print_redirect(prev_shared_print);
        crate::io::set_shared_stdout_redirect(prev_shared_stdout);
        crate::io::set_shared_stderr_redirect(prev_shared_stderr);
    }
    crate::io::restore_captures(prev_captures);
    drop(job_scope);
    let mut stored = match result {
        Ok(Ok(pv)) => Ok(pv),
        Ok(Err(tag)) => Err(tag),
        Err(_) => Err(ThreadErrorTag::JoinFailed),
    };
    if state.cancelled() {
        // Stopped part way: whatever it left is no result (#2442).
        stored = Err(ThreadErrorTag::JoinFailed);
        panic_message = None;
    } else if speculative && stored.is_err() {
        // Its fork-join failed: the arms or chunks after it will not be
        // used. Before the result is stored, so before the fork-join goes
        // on past its join.
        state.cancel_later();
    }
    // The next job on this runtime starts with no budget.
    rt.fuel = u64::MAX;
    rt.step_budget_hit = false;
    if shared {
        // Before the joiner can see the result and close the epoch.
        rt.heap.get_mut().flush_epoch_batch();
    }
    state.store_result_with_panic(stored, panic_message);
    reactor.job_done();
    rt.native = None;
    rt.output = None;
    if shared {
        rt.statics.clear();
        rt.heap.unbind();
        rt.shared_epoch = None;
        rt.reset_shared();
    } else {
        rt.reset_isolate();
    }
}
