//! Discovery, per-file compile, and per-case execution.

use std::io::Write;
use std::path::{Path, PathBuf};

use std::sync::{Arc, Mutex};

use coil_host::{
    ExecutePipelineArgs, bind_cli_roots, execute_pipeline, wire_pipeline_threads, wire_pipeline_vm,
};
use common::{Byte, Instruction, ProgramDebug};
use compiler::{HostGrants, OptLevel, Pipeline};
use machine::Machine;
use machine::reactor::{Reactor, TestCase, TestHandle, TestReport};
use machine::thread::ThreadSpawnContext;
use reporting::{ErrorCode, ReportConfig, ReportFormat};

use crate::coverage::{Coverage, CoverageOptions, ProgramLines, project_filter, test_fn_ranges};
use crate::events::{self, Event};
use crate::order::{Order, format_seed};

/// How a run reports its progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Report {
    /// Text on stderr.
    #[default]
    Human,
    /// NDJSON events on stdout (`--json`).
    Json,
    /// Nothing (the `coil mutate --json` baseline).
    Silent,
}

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
    /// Line coverage (`--coverage`); `None` = off.
    pub coverage: Option<CoverageOptions>,
    pub opt_level: OptLevel,
    /// `--contracts`; tests check every clause unless told otherwise.
    pub contracts: Option<compiler::ContractLevel>,
    pub grants: HostGrants,
    /// Extra `--root` module search directories.
    pub extra_roots: Vec<PathBuf>,
    /// Text, `--json` events, or nothing.
    pub report: Report,
}

pub(crate) fn writer_for(format: ReportFormat) -> Box<dyn Write + Send> {
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

/// Error codes (`E0209`) a `compile_fail/` file declares on the `// Expected:`
/// lines of its leading comment header.
pub fn declared_error_codes(src: &str) -> Vec<String> {
    src.lines()
        .take_while(|l| l.starts_with("//"))
        .filter(|l| l.contains("Expected"))
        .flat_map(error_codes_in)
        .collect()
}

/// Every `E` + four digits in `text`, in order.
fn error_codes_in(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut codes = Vec::new();
    for i in 0..bytes.len() {
        let code = bytes.get(i..i + 5);
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if let Some(code) = code
            && boundary
            && code[0] == b'E'
            && code[1..].iter().all(u8::is_ascii_digit)
            && bytes.get(i + 5).is_none_or(|b| !b.is_ascii_alphanumeric())
        {
            codes.push(String::from_utf8_lossy(code).into_owned());
        }
    }
    codes
}

/// A rejected `compile_fail/` file passes when its diagnostics include one of
/// the error codes its header declares, so a test cannot keep passing after
/// it starts failing for an unrelated reason.
fn compile_fail_verdict(src: &str, reported: &str) -> (bool, Option<String>) {
    let declared = declared_error_codes(src);
    if declared.is_empty() {
        return (
            false,
            Some("no expected error code: start the file with `// Expected: E…`".to_string()),
        );
    }
    let mut got = error_codes_in(reported);
    got.dedup();
    if got.iter().any(|c| declared.contains(c)) {
        (true, None)
    } else {
        let got = if got.is_empty() {
            "none".to_string()
        } else {
            got.join(", ")
        };
        (
            false,
            Some(format!(
                "expected {}, compiler reported {got}",
                declared.join(" or ")
            )),
        )
    }
}

/// Classify a `catch_unwind` compile result for a `compile_fail/` file.
/// Only a clean diagnostic rejection (`Ok(Err(_))`) is harness success.
/// Panic does not count (release builds use `panic = "abort"`).
fn compile_fail_rejected<T, E>(compiled: &std::thread::Result<Result<T, E>>) -> bool {
    matches!(compiled, Ok(Err(_)))
}

/// Counts plus the order files were started in.
#[derive(Default)]
pub struct SuiteResult {
    pub passed: usize,
    pub failed: usize,
    pub files_run: Vec<PathBuf>,
    /// Summed line coverage when `--coverage` was on.
    pub coverage: Option<Coverage>,
    /// Every case's outcome, in report order.
    pub cases: Vec<CaseOutcome>,
}

/// One case's verdict, as `coil mutate` needs it from the baseline run.
#[derive(Debug, Clone)]
pub struct CaseOutcome {
    /// Test file, as the runner was given it.
    pub file: PathBuf,
    pub name: String,
    pub passed: bool,
    /// Step-budget charges (see `Machine::set_step_budget`).
    pub steps: u64,
    /// Covered project lines per canonical source path (with `--coverage`).
    pub covered: Vec<(PathBuf, Vec<u32>)>,
}

/// What every file of one run shares.
struct Run<'a> {
    config: &'a ReportConfig,
    options: &'a TestOptions,
    reactor: &'a Arc<Reactor>,
    coverage: Option<&'a Mutex<Coverage>>,
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
    path: PathBuf,
    display: String,
    /// Compiler diagnostics, captured so parallel compiles do not interleave.
    diagnostics: Captured,
    /// File-level verdict (compile outcome, `compile_fail/`); `None` when
    /// the verdict is its cases'.
    verdict: Option<(bool, Option<String>)>,
    cases: Vec<PendingCase>,
    /// PC → source line map for recording this program's coverage.
    lines: Option<ProgramLines>,
}

impl PendingFile {
    fn decided(path: &Path, diagnostics: Captured, verdict: (bool, Option<String>)) -> Self {
        PendingFile {
            path: path.to_path_buf(),
            display: path.display().to_string(),
            diagnostics,
            verdict: Some(verdict),
            cases: Vec::new(),
            lines: None,
        }
    }
}

/// Shared byte buffer usable as a `Pipeline` report sink.
#[derive(Clone, Default)]
pub(crate) struct Captured(Arc<Mutex<Vec<u8>>>);

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

/// Wait for a file's cases, print its report, and add it to `result`.
/// Returns the file's failed count.
fn finish_file(run: &Run<'_>, file: PendingFile, result: &mut SuiteResult) -> usize {
    let report = run.options.report;
    let diagnostics = file.diagnostics.take();
    if !diagnostics.is_empty() && report == Report::Human {
        let _ = writer_for(run.config.format).write_all(&diagnostics);
    }
    let (mut passed, mut failed) = (0, 0);
    let mut message = None;
    if let Some((ok, m)) = file.verdict {
        if report == Report::Human
            && let Some(m) = &m
        {
            eprintln!("{m}");
        }
        message = m;
        if ok {
            passed += 1;
        } else {
            failed += 1;
        }
    }
    let mut case_events = Vec::new();
    for case in file.cases {
        let TestReport {
            passed: report_ok,
            reason,
            hits,
            steps,
            timed_out,
        } = match case.state {
            CaseState::Running(handle) => handle.wait(),
            CaseState::Done(report) => report,
        };
        let ok = report_ok && !timed_out;
        let covered = match (run.coverage, &file.lines, hits) {
            (Some(cov), Some(lines), Some(hits)) => cov
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record(lines, &hits, &file.display, &case.name),
            _ => Vec::new(),
        };
        result.cases.push(CaseOutcome {
            file: file.path.clone(),
            name: case.name.clone(),
            passed: ok,
            steps,
            covered,
        });
        let output = std::mem::take(&mut *case.output.lock().unwrap_or_else(|e| e.into_inner()));
        let show = !ok || run.options.show_output;
        if ok {
            passed += 1;
        } else {
            failed += 1;
        }
        match report {
            Report::Human => {
                if show {
                    print_captured(&output);
                }
                if !ok {
                    match (timed_out, &reason) {
                        (true, _) => eprintln!("> Test \"{}\" failed (timed out)", case.name),
                        // `assert(cond, "message")?` returns `Err("message")`.
                        (false, Some(reason)) => {
                            eprintln!("> Test \"{}\" failed: {reason}", case.name)
                        }
                        (false, None) => eprintln!("> Test \"{}\" failed", case.name),
                    }
                }
            }
            Report::Json => {
                let text = String::from_utf8_lossy(&output);
                case_events.push(
                    Event::object()
                        .str("name", &case.name)
                        .bool("ok", ok)
                        .bool("timed_out", timed_out)
                        .opt_str("reason", reason.as_deref())
                        .opt_str("output", (show && !text.is_empty()).then_some(&*text))
                        .finish(),
                );
            }
            Report::Silent => {}
        }
    }
    match report {
        Report::Human => {
            if failed == 0 {
                eprintln!("ok   {}", file.display);
            } else {
                eprintln!("FAILED {}", file.display);
            }
        }
        Report::Json => {
            let diagnostics = String::from_utf8_lossy(&diagnostics);
            Event::new("file")
                .str("file", &file.display)
                .bool("ok", failed == 0)
                .num("passed", passed)
                .num("failed", failed)
                .opt_str("message", message.as_deref())
                .opt_str(
                    "diagnostics",
                    (!diagnostics.is_empty()).then_some(&*diagnostics),
                )
                .raw("cases", &events::array(&case_events))
                .emit();
        }
        Report::Silent => {}
    }
    result.passed += passed;
    result.failed += failed;
    failed
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

/// A test file compiled and wired, ready to run its cases.
pub(crate) struct Prepared {
    pub ctx: ThreadSpawnContext,
    /// `(name, entry)` in run order. A file without cases runs `main` once
    /// as a single case named after the file.
    pub cases: Vec<(String, u32)>,
    pub init_ip: Option<u32>,
    /// See [`TestCase::expect_ok_result`].
    pub expect_ok_result: bool,
    pub debug: ProgramDebug,
}

impl Prepared {
    pub fn case(&self, entry: u32, coverage: bool, step_budget: Option<u64>) -> TestCase {
        TestCase {
            entry,
            init_ip: self.init_ip,
            expect_ok_result: self.expect_ok_result,
            coverage,
            step_budget,
        }
    }
}

/// A test file's compile result.
pub(crate) enum Compiled {
    /// The file's verdict is its compile (`compile_fail/`, errors, a file
    /// without cases or `main` run from the top): `(ok, message)`.
    Decided(bool, Option<String>),
    Ready(Box<Prepared>),
}

/// Compile one test file with `overlays` (path spelling → text) standing in
/// for sources on disk. Diagnostics go to `diagnostics`; `None` drops them.
pub(crate) fn compile_test_file(
    config: &ReportConfig,
    options: &TestOptions,
    reactor: &Arc<Reactor>,
    path: &Path,
    diagnostics: Option<&Captured>,
    overlays: &[(PathBuf, String)],
) -> Compiled {
    let display = path.display().to_string();
    let expect_compile_fail = is_compile_fail(path);
    // Expected compile rejection: suppress ariadne noise so the harness
    // summary stays readable when many compile_fail files exist.
    // A rejection's diagnostics are kept apart: the case checks their error
    // codes against the file's `// Expected:` header instead of printing them.
    let rejection = Captured::default();
    let mut pipeline = match diagnostics {
        Some(d) if !expect_compile_fail => {
            Pipeline::with_reporter(config.clone(), Box::new(d.clone()))
        }
        _ if expect_compile_fail => {
            Pipeline::with_reporter(config.clone(), Box::new(rejection.clone()))
        }
        _ => Pipeline::with_reporter(config.clone(), Box::new(std::io::sink())),
    };
    pipeline.set_include_tests(true);
    pipeline.set_opt_level(options.opt_level);
    pipeline.set_contracts(options.contracts.unwrap_or(compiler::ContractLevel::All));
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
    if let Some(cov) = &options.coverage {
        // Keep never-called project functions so they report as uncovered.
        pipeline.set_keep_fns_in(Some(project_filter(canonical(&cov.project_root))));
    }
    for (file, text) in overlays {
        pipeline.set_file_text(file.clone(), text.clone());
    }

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
        let (ok, why) = match &compiled {
            _ if compile_fail_rejected(&compiled) => {
                let src = std::fs::read_to_string(path).unwrap_or_default();
                let reported = String::from_utf8_lossy(&rejection.take()).into_owned();
                compile_fail_verdict(&src, &reported)
            }
            Ok(Ok(_)) => (false, Some("expected compile failure".to_string())),
            _ => (false, Some("compiler panicked".to_string())),
        };
        let message = why.map(|why| format!("> Test \"{display}\" failed ({why})"));
        return Compiled::Decided(ok, message);
    }
    let (bytecode, constants) = match compiled {
        Err(_) => {
            let m = format!("> Test \"{display}\" failed (compiler panicked)");
            return Compiled::Decided(false, Some(m));
        }
        Ok(Err(_)) => {
            return Compiled::Decided(false, Some(format!("> Test \"{display}\" failed")));
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
        return Compiled::Decided(ok, (!ok).then(|| format!("> Test \"{display}\" failed")));
    }

    // Stop the static-init prologue before it jumps into `main`; each job
    // runs it on a fresh VM, then calls its case.
    let mut code = bytecode;
    let init_ip = main.and_then(|main| {
        let setup = pipeline.prologue_jmp_target();
        (setup != main && halt_first_jump_in(&mut code, setup as usize, main)).then_some(setup)
    });
    let debug = pipeline.program_debug();
    let ctx = {
        let mut root = Machine::<256>::default();
        wire_pipeline_vm(&pipeline, &mut root, Some(path));
        // The same precise frame / class / static word maps as `coil <file>`,
        // so a case's `collect()` exercises production GC roots (#555).
        wire_pipeline_threads(&pipeline, &mut root, &code, &constants, &strings);
        root.set_program_debug(debug.clone());
        root.set_reactor(Arc::clone(reactor));
        root.thread_spawn_context()
    };
    drop(pipeline);
    let Some(ctx) = ctx else {
        let m = format!("> Test \"{display}\" failed (no thread program)");
        return Compiled::Decided(false, Some(m));
    };

    // A file without cases runs `main` once as a single opaque case, where
    // only a panic fails (an uncaught `raise` from `main` is an `Err` return).
    let (cases, expect_ok_result) = if cases.is_empty() {
        (vec![(display, main.unwrap_or(0))], false)
    } else {
        (cases, true)
    };
    Compiled::Ready(Box::new(Prepared {
        ctx,
        cases,
        init_ip,
        expect_ok_result,
        debug,
    }))
}

/// Compile one file, then decide it or start its cases.
fn start_file(run: &Run<'_>, dispatch: Dispatch, path: &Path) -> PendingFile {
    let diagnostics = Captured::default();
    let compiled = compile_test_file(
        run.config,
        run.options,
        run.reactor,
        path,
        Some(&diagnostics),
        &[],
    );
    let prepared = match compiled {
        Compiled::Decided(ok, message) => {
            return PendingFile::decided(path, diagnostics, (ok, message));
        }
        Compiled::Ready(prepared) => prepared,
    };
    let lines = run.coverage.map(|cov| {
        let tests = test_fn_ranges(
            &prepared.debug,
            prepared.cases.iter().map(|(_, entry)| *entry),
        );
        cov.lock()
            .unwrap_or_else(|e| e.into_inner())
            .register_program(&prepared.debug, &tests)
    });
    let cases = prepared
        .cases
        .iter()
        .map(|(name, entry)| {
            let case = prepared.case(*entry, run.coverage.is_some(), None);
            let output = Arc::new(Mutex::new(Vec::new()));
            let print = Arc::clone(&output);
            let ctx = prepared.ctx.clone();
            let state = match dispatch {
                Dispatch::Pool => CaseState::Running(run.reactor.submit_test(ctx, case, print)),
                Dispatch::Inline => CaseState::Done(run.reactor.run_test_here(ctx, case, print)),
            };
            PendingCase {
                name: name.clone(),
                state,
                output,
            }
        })
        .collect();
    PendingFile {
        path: path.to_path_buf(),
        display: path.display().to_string(),
        diagnostics,
        verdict: None,
        cases,
        lines,
    }
}

/// Run the harness over `options.root` without exiting.
pub fn run_test_suite(config: ReportConfig, options: &TestOptions) -> Result<SuiteResult, String> {
    let mut files = collect_test_files(&options.root)?;
    options.order.order_files(&mut files);
    let jobs = options.jobs.max(1);
    match options.report {
        Report::Human => eprintln!(
            "running {} file{} ({}, {} job{})",
            files.len(),
            if files.len() == 1 { "" } else { "s" },
            options.order.describe(),
            jobs,
            if jobs == 1 { "" } else { "s" },
        ),
        Report::Json => Event::new("start")
            .num("files", files.len())
            .opt_str("seed", seed_of(options.order).as_deref())
            .num("jobs", jobs)
            .emit(),
        Report::Silent => {}
    }

    let reactor = Reactor::new(jobs);
    let coverage = options.coverage.as_ref().map(|c| {
        Mutex::new(Coverage::new(
            canonical(&c.project_root),
            c.per_test_out.is_some(),
        ))
    });
    let run = Run {
        config: &config,
        options,
        reactor: &reactor,
        coverage: coverage.as_ref(),
    };
    let mut result = if jobs == 1 {
        run_serial(&run, &files)
    } else {
        run_parallel(&run, &files, jobs)
    };
    reactor.shutdown();
    result.coverage = coverage.map(|c| c.into_inner().unwrap_or_else(|e| e.into_inner()));
    Ok(result)
}

/// `--jobs 1`: compile and run each file on this thread, one at a time. No
/// pool worker starts unless a case spawns threads.
fn run_serial(run: &Run<'_>, files: &[PathBuf]) -> SuiteResult {
    let mut result = SuiteResult::default();
    for path in files {
        result.files_run.push(path.clone());
        let file = start_file(run, Dispatch::Inline, path);
        let failed = finish_file(run, file, &mut result);
        if failed != 0 && run.options.fail_fast {
            break;
        }
    }
    result
}

/// Compile threads claim files in order and queue their cases on the pool;
/// this thread reports files in order, helping run cases while it waits.
fn run_parallel(run: &Run<'_>, files: &[PathBuf], jobs: usize) -> SuiteResult {
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
                        let file = start_file(run, Dispatch::Pool, &files[index]);
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
                let failed = finish_file(run, file, &mut result);
                next += 1;
                if failed != 0 && run.options.fail_fast {
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

/// The `--seed` value that reproduces this order (`None` when sorted).
fn seed_of(order: Order) -> Option<String> {
    match order {
        Order::Shuffled(seed) => Some(format_seed(seed)),
        Order::Sorted => None,
    }
}

pub(crate) fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Write `text` to `path`, creating parent directories.
pub(crate) fn write_file(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)
}

/// Process status for a finished suite: non-zero iff any case failed.
pub fn suite_exit_code(failed: usize) -> i32 {
    if failed == 0 { 0 } else { 1 }
}

/// `coil test` entry: run the suite, print the summary, return the process status.
///
/// A `test result: FAILED` summary always returns 1. Callers must `exit` that
/// value — falling off `main` after a red summary would green CI.
pub fn cmd_test(config: ReportConfig, options: TestOptions) -> i32 {
    let json = options.report == Report::Json;
    let SuiteResult {
        passed,
        failed,
        coverage,
        ..
    } = match run_test_suite(config.clone(), &options) {
        Ok(result) => result,
        Err(msg) => {
            if json {
                events::emit_error(&msg);
                return 1;
            }
            let format = config.format;
            let mut pipeline = Pipeline::with_reporter(config, writer_for(format));
            pipeline.emit_spanless_error(ErrorCode::IoError, msg);
            let _ = pipeline.finish_reporting();
            return 1;
        }
    };

    if !json {
        eprintln!();
        eprintln!(
            "test result: {}. {passed} passed; {failed} failed; {} total",
            if failed == 0 { "ok" } else { "FAILED" },
            passed + failed
        );
    }
    let mut coverage_event = "null".to_string();
    if let (Some(cov), Some(out)) = (&coverage, &options.coverage) {
        if !json {
            eprintln!();
            eprint!("{}", cov.summary());
        }
        let lcov = out.lcov_out.display().to_string();
        let mut lcov_written = true;
        if let Err(e) = write_file(&out.lcov_out, &cov.lcov()) {
            lcov_written = false;
            report_problem(json, &format!("coverage: cannot write `{lcov}`: {e}"));
        } else if !json {
            eprintln!("coverage: lcov written to {lcov}");
        }
        if let (Some(path), Some(per_test)) = (&out.per_test_out, cov.per_test_json())
            && let Err(e) = write_file(path, &per_test)
        {
            report_problem(
                json,
                &format!("coverage: cannot write `{}`: {e}", path.display()),
            );
        }
        let (mut hit, mut total) = (0usize, 0usize);
        let files: Vec<String> = cov
            .file_totals()
            .into_iter()
            .map(|(path, h, t)| {
                hit += h;
                total += t;
                Event::object()
                    .str("file", &path)
                    .num("hit", h)
                    .num("total", t)
                    .finish()
            })
            .collect();
        coverage_event = Event::object()
            .num("hit", hit)
            .num("total", total)
            .opt_str("lcov", lcov_written.then_some(lcov.as_str()))
            .raw("files", &events::array(&files))
            .finish();
    }

    if json {
        Event::new("summary")
            .bool("ok", failed == 0)
            .num("passed", passed)
            .num("failed", failed)
            .num("total", passed + failed)
            .opt_str("seed", seed_of(options.order).as_deref())
            .raw("coverage", &coverage_event)
            .emit();
    } else if failed != 0
        && let Order::Shuffled(seed) = options.order
    {
        eprintln!("rerun in this order with `--seed {}`", format_seed(seed));
    }
    suite_exit_code(failed)
}

/// A non-fatal problem: an `error` event under `--json`, else stderr.
fn report_problem(json: bool, message: &str) {
    if json {
        events::emit_error(message);
    } else {
        eprintln!("{message}");
    }
}

#[cfg(test)]
#[path = "runner.tests.rs"]
mod tests;
