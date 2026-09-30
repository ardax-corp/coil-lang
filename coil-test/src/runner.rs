//! Discovery, per-file compile, and per-case execution.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;

use std::sync::{Arc, Mutex};

use coil_host::{ExecutePipelineArgs, bind_cli_roots, execute_pipeline, wire_pipeline_vm};
use common::{Byte, Instruction};
use compiler::{HostGrants, OptLevel, Pipeline};
use machine::reactor::{Reactor, TestCase, TestHandle, TestReport};
use machine::{Machine, wire_thread_program};
use reporting::{ErrorCode, ReportConfig, ReportFormat};

use crate::order::{Order, format_seed};

/// What to run and how to compile it.
#[derive(Debug, Clone)]
pub struct TestOptions {
    /// Test root (a directory walked for `.hy` files).
    pub root: PathBuf,
    /// Stop after the first failed case.
    pub fail_fast: bool,
    /// File and case order (`--seed` / `--no-shuffle`).
    pub order: Order,
    /// Reactor workers running cases in parallel (`--jobs`, at least 1).
    pub jobs: usize,
    /// Also print passing cases' captured output (`--show-output`).
    pub show_output: bool,
    pub opt_level: OptLevel,
    pub grants: HostGrants,
    /// Extra `--root` module search directories.
    pub extra_roots: Vec<PathBuf>,
}

fn writer_for(format: ReportFormat) -> Box<dyn Write + Send> {
    match format {
        ReportFormat::Pretty => Box::new(std::io::stderr()),
        ReportFormat::Sarif | ReportFormat::Lsp => Box::new(std::io::stdout()),
    }
}

/// Every `.hy` file under `dir`, recursively, sorted by full path.
pub fn collect_test_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Err(format!("tests directory `{}` not found", dir.display()));
    }
    let mut files = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("unable to read `{}`: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("unable to read directory entry: {e}"))?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if path.extension().and_then(|e| e.to_str()) == Some("hy") {
                out.push(path);
            }
        }
        Ok(())
    }
    walk(dir, &mut files)?;
    files.sort();
    if files.is_empty() {
        return Err(format!(
            "no `.hy` test files found under `{}`",
            dir.display()
        ));
    }
    Ok(files)
}

/// Negative syntax / type tests live under any path segment named `compile_fail`.
/// Those files must fail to compile; a successful compile is a harness failure.
pub fn is_compile_fail(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "compile_fail")
}

/// Classify a `catch_unwind` compile result for a `compile_fail/` file.
/// Only a clean diagnostic rejection (`Ok(Err(_))`) is harness success.
/// Panic does not count (release builds use `panic = "abort"`).
fn compile_fail_rejected<T, E>(compiled: &std::thread::Result<Result<T, E>>) -> bool {
    matches!(compiled, Ok(Err(_)))
}

/// Counts plus the order files were started in.
#[derive(Debug, Default)]
pub struct SuiteResult {
    pub passed: usize,
    pub failed: usize,
    pub files_run: Vec<PathBuf>,
}

/// A case's verdict, or the handle to wait on for it.
enum CaseState {
    Running(TestHandle),
    Done(TestReport),
}

/// One case with its captured output.
struct PendingCase {
    name: String,
    state: CaseState,
    output: Arc<Mutex<Vec<u8>>>,
}

/// A file as the runner reports it. Files print in start order, so output
/// is the same for a given seed whatever `--jobs` is.
struct PendingFile {
    display: String,
    /// Compiler diagnostics, captured so parallel compiles do not interleave.
    diagnostics: Captured,
    /// File-level verdict (compile outcome, `compile_fail/`); `None` when
    /// the verdict is its cases'.
    verdict: Option<(bool, Option<String>)>,
    cases: Vec<PendingCase>,
}

impl PendingFile {
    fn decided(display: String, diagnostics: Captured, ok: bool, message: Option<String>) -> Self {
        PendingFile {
            display,
            diagnostics,
            verdict: Some((ok, message)),
            cases: Vec::new(),
        }
    }
}

/// Shared byte buffer usable as a `Pipeline` report sink.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Wait for a file's cases, print its report, and return `(passed, failed)`.
fn finish_file(file: PendingFile, format: ReportFormat, show_output: bool) -> (usize, usize) {
    let diagnostics = file.diagnostics.take();
    if !diagnostics.is_empty() {
        let _ = writer_for(format).write_all(&diagnostics);
    }
    let (mut passed, mut failed) = (0, 0);
    if let Some((ok, message)) = file.verdict {
        if let Some(m) = message {
            eprintln!("{m}");
        }
        if ok {
            passed += 1;
        } else {
            failed += 1;
        }
    }
    for case in file.cases {
        let TestReport { passed: ok, reason } = match case.state {
            CaseState::Running(handle) => handle.wait(),
            CaseState::Done(report) => report,
        };
        let output = std::mem::take(&mut *case.output.lock().unwrap_or_else(|e| e.into_inner()));
        if !ok || show_output {
            print_captured(&output);
        }
        if ok {
            passed += 1;
        } else {
            failed += 1;
            match reason {
                // `assert(cond, "message")?` returns `Err("message")`.
                Some(reason) => eprintln!("> Test \"{}\" failed: {reason}", case.name),
                None => eprintln!("> Test \"{}\" failed", case.name),
            }
        }
    }
    if failed == 0 {
        eprintln!("ok   {}", file.display);
    } else {
        eprintln!("FAILED {}", file.display);
    }
    (passed, failed)
}

/// Replay a case's prints / panic message on stderr, next to its verdict.
fn print_captured(output: &[u8]) {
    if output.is_empty() {
        return;
    }
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(output);
    if !output.ends_with(b"\n") {
        let _ = err.write_all(b"\n");
    }
}

/// Replace the first `JMP target` at or after `from` with `HALT` (the static-init
/// prologue's jump into `main`), like [`Machine::halt_first_jump_to`] on a copy.
fn halt_first_jump_in(code: &mut [Byte], from: usize, target: u32) -> bool {
    for b in code.iter_mut().skip(from) {
        if matches!(b.bytecode(), Instruction::JMP) && b.operand_u32() == target {
            *b = Byte::new(Instruction::HALT);
            return true;
        }
    }
    false
}

/// Queue cases on the pool, or run them right here (`--jobs 1`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dispatch {
    Pool,
    Inline,
}

/// Compile one file, then decide it or start its cases.
fn start_file(
    config: &ReportConfig,
    options: &TestOptions,
    reactor: &Arc<Reactor>,
    dispatch: Dispatch,
    path: &Path,
) -> PendingFile {
    let display = path.display().to_string();
    let expect_compile_fail = is_compile_fail(path);
    let diagnostics = Captured::default();
    // Expected compile rejection: suppress ariadne noise so the harness
    // summary stays readable when many compile_fail files exist.
    let mut pipeline = if expect_compile_fail {
        Pipeline::with_reporter(config.clone(), Box::new(std::io::sink()))
    } else {
        Pipeline::with_reporter(config.clone(), Box::new(diagnostics.clone()))
    };
    pipeline.set_include_tests(true);
    pipeline.set_opt_level(options.opt_level);
    pipeline.set_host_grants(options.grants.clone());
    // Same search path CI passes with `--root`: examples and a sibling
    // coil-stdlib checkout, when those directories exist.
    let mut roots = options.extra_roots.clone();
    for extra in compiler::Pipeline::workspace_language_extra_roots() {
        if extra.is_dir() && !roots.contains(&extra) {
            roots.push(extra);
        }
    }
    bind_cli_roots(&mut pipeline, roots);

    // catch_unwind isolates a compiler ICE from aborting the whole
    // harness under panic=unwind. Release builds use panic=abort, so
    // compile_fail fixtures must reject via Ok(Err(_)), not panic.
    let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pipeline.compile_src_from_file(&display)
    }));
    let mut cases: Vec<(String, u32)> = pipeline.test_cases().to_vec();
    options.order.order_cases(&options.root, path, &mut cases);
    let _ = pipeline.finish_reporting();

    if expect_compile_fail {
        // Only a clean diagnostic rejection counts. A panic is a
        // harness failure (and aborts under release panic=abort).
        let (ok, message) = match &compiled {
            _ if compile_fail_rejected(&compiled) => (true, None),
            Ok(Ok(_)) => (false, Some("expected compile failure")),
            _ => (false, Some("compiler panicked")),
        };
        let message = message.map(|why| format!("> Test \"{display}\" failed ({why})"));
        return PendingFile::decided(display, diagnostics, ok, message);
    }
    let (bytecode, constants) = match compiled {
        Err(_) => {
            let m = format!("> Test \"{display}\" failed (compiler panicked)");
            return PendingFile::decided(display, diagnostics, false, Some(m));
        }
        Ok(Err(_)) => {
            let m = format!("> Test \"{display}\" failed");
            return PendingFile::decided(display, diagnostics, false, Some(m));
        }
        Ok(Ok(ok)) => ok,
    };
    let strings = pipeline.strings().to_vec();
    let main = pipeline.main_offset();

    if cases.is_empty() && main.is_none() {
        // No cases and no `main` entry: run the program from the top, as
        // `coil <file>` would, on this thread.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute_pipeline(ExecutePipelineArgs {
                pipeline: &pipeline,
                bytecode: &bytecode,
                constants: &constants,
                strings: &strings,
                static_slots: pipeline.static_slot_count(),
                debug: pipeline.program_debug(),
                entry: Some(path),
                operand_stack_slots: pipeline.operand_stack_slots(),
            })
        }));
        let ok = matches!(result, Ok(false));
        let m = (!ok).then(|| format!("> Test \"{display}\" failed"));
        return PendingFile::decided(display, diagnostics, ok, m);
    }

    // Stop the static-init prologue before it jumps into `main`; each job
    // runs it on a fresh VM, then calls its case.
    let mut code = bytecode;
    let init_ip = main.and_then(|main| {
        let setup = pipeline.prologue_jmp_target();
        (setup != main && halt_first_jump_in(&mut code, setup as usize, main)).then_some(setup)
    });
    let ctx = {
        let mut root = Machine::<256>::default();
        wire_pipeline_vm(&pipeline, &mut root, Some(path));
        // No precise frame / class / static word maps: cases scan
        // conservatively, as the harness always has. With the maps, `collect()`
        // can free a live `Result::Err(obj)` payload (same under `coil <file>`);
        // switch to `wire_pipeline_threads` once that GC bug is fixed.
        wire_thread_program(
            &mut root,
            &code,
            &constants,
            &strings,
            pipeline.static_slot_count(),
            pipeline.program_debug(),
            pipeline.operand_stack_slots(),
        );
        root.set_program_debug(pipeline.program_debug());
        root.set_reactor(Arc::clone(reactor));
        root.thread_spawn_context()
    };
    drop(pipeline);
    let Some(ctx) = ctx else {
        let m = format!("> Test \"{display}\" failed (no thread program)");
        return PendingFile::decided(display, diagnostics, false, Some(m));
    };

    // A file without cases runs `main` once as a single opaque case, where
    // only a panic fails (an uncaught `raise` from `main` is an `Err` return).
    let (targets, expect_ok_result) = if cases.is_empty() {
        (vec![(display.clone(), main.unwrap_or(0))], false)
    } else {
        (cases, true)
    };
    let cases = targets
        .into_iter()
        .map(|(name, entry)| {
            let case = TestCase {
                entry,
                init_ip,
                expect_ok_result,
            };
            let output = Arc::new(Mutex::new(Vec::new()));
            let print = Arc::clone(&output);
            let state = match dispatch {
                Dispatch::Pool => CaseState::Running(reactor.submit_test(ctx.clone(), case, print)),
                Dispatch::Inline => {
                    CaseState::Done(reactor.run_test_here(ctx.clone(), case, print))
                }
            };
            PendingCase {
                name,
                state,
                output,
            }
        })
        .collect();
    PendingFile {
        display,
        diagnostics,
        verdict: None,
        cases,
    }
}

/// Run the harness over `options.root` without exiting.
pub fn run_test_suite(config: ReportConfig, options: &TestOptions) -> Result<SuiteResult, String> {
    let mut files = collect_test_files(&options.root)?;
    options.order.order_files(&mut files);
    let jobs = options.jobs.max(1);
    eprintln!(
        "running {} file{} ({}, {} job{})",
        files.len(),
        if files.len() == 1 { "" } else { "s" },
        options.order.describe(),
        jobs,
        if jobs == 1 { "" } else { "s" },
    );

    let reactor = Reactor::new(jobs);
    let result = if jobs == 1 {
        run_serial(&config, options, &reactor, &files)
    } else {
        run_parallel(&config, options, &reactor, &files, jobs)
    };
    reactor.shutdown();
    Ok(result)
}

/// `--jobs 1`: compile and run each file on this thread, one at a time. No
/// pool worker starts unless a case spawns threads.
fn run_serial(
    config: &ReportConfig,
    options: &TestOptions,
    reactor: &Arc<Reactor>,
    files: &[PathBuf],
) -> SuiteResult {
    let mut result = SuiteResult::default();
    for path in files {
        result.files_run.push(path.clone());
        let file = start_file(config, options, reactor, Dispatch::Inline, path);
        let (passed, failed) = finish_file(file, config.format, options.show_output);
        result.passed += passed;
        result.failed += failed;
        if failed != 0 && options.fail_fast {
            break;
        }
    }
    result
}

/// Compile threads claim files in order and queue their cases on the pool;
/// this thread reports files in order, helping run cases while it waits.
fn run_parallel(
    config: &ReportConfig,
    options: &TestOptions,
    reactor: &Arc<Reactor>,
    files: &[PathBuf],
    jobs: usize,
) -> SuiteResult {
    use std::sync::Condvar;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    // A compile thread may start file `i` only once `i < reported + window`,
    // so a bounded number of compiled programs is alive at a time.
    let window = jobs * 2;
    let progress = Mutex::new((0usize, 0usize)); // (next to claim, reported)
    let advanced = Condvar::new();
    let stop = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel::<(usize, PendingFile)>();
    let mut result = SuiteResult::default();

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            let tx = tx.clone();
            let (progress, advanced, stop) = (&progress, &advanced, &stop);
            std::thread::Builder::new()
                .name("coil-test-compile".into())
                // Deep ASTs recurse in the front end; match the main thread.
                .stack_size(8 * 1024 * 1024)
                .spawn_scoped(scope, move || {
                    loop {
                        let index = {
                            let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
                            loop {
                                if stop.load(Ordering::SeqCst) || p.0 >= files.len() {
                                    return;
                                }
                                if p.0 < p.1 + window {
                                    break;
                                }
                                p = advanced.wait(p).unwrap_or_else(|e| e.into_inner());
                            }
                            p.0 += 1;
                            p.0 - 1
                        };
                        let file =
                            start_file(config, options, reactor, Dispatch::Pool, &files[index]);
                        if tx.send((index, file)).is_err() {
                            return;
                        }
                    }
                })
                .expect("spawn coil-test compile thread");
        }
        drop(tx);

        let mut ready: std::collections::BTreeMap<usize, PendingFile> = Default::default();
        let mut next = 0usize;
        for (index, file) in rx.iter() {
            ready.insert(index, file);
            while let Some(file) = ready.remove(&next) {
                result.files_run.push(files[next].clone());
                let (passed, failed) = finish_file(file, config.format, options.show_output);
                result.passed += passed;
                result.failed += failed;
                next += 1;
                if failed != 0 && options.fail_fast {
                    // Files already claimed still finish and are reported.
                    stop.store(true, Ordering::SeqCst);
                }
                progress.lock().unwrap_or_else(|e| e.into_inner()).1 = next;
                advanced.notify_all();
            }
        }
    });
    result
}

/// `coil test` entry: run the suite, print the summary, exit non-zero on failure.
pub fn cmd_test(config: ReportConfig, options: TestOptions) {
    let SuiteResult { passed, failed, .. } = match run_test_suite(config.clone(), &options) {
        Ok(result) => result,
        Err(msg) => {
            let format = config.format;
            let mut pipeline = Pipeline::with_reporter(config, writer_for(format));
            pipeline.emit_spanless_error(ErrorCode::IoError, msg);
            let _ = pipeline.finish_reporting();
            exit(1);
        }
    };

    eprintln!();
    eprintln!(
        "test result: {}. {passed} passed; {failed} failed; {} total",
        if failed == 0 { "ok" } else { "FAILED" },
        passed + failed
    );

    if failed != 0 {
        if let Order::Shuffled(seed) = options.order {
            eprintln!("rerun in this order with `--seed {}`", format_seed(seed));
        }
        exit(1);
    }
}

#[cfg(test)]
#[path = "runner.tests.rs"]
mod tests;
