//! End-to-end golden tests for `.coil` example programs.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

/// Absolute path to coil-stdlib `src/` (sibling clone or `.deps/` checkout).
fn workspace_stdlib() -> PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate parent");
    let candidates = [
        workspace.join(".deps/coil-stdlib/src"),
        workspace
            .parent()
            .unwrap_or(workspace)
            .join("coil-stdlib/src"),
    ];
    for c in candidates {
        if c.is_dir() {
            return c;
        }
    }
    workspace.join(".deps/coil-stdlib/src")
}

use compiler::native_mir::{MirAllocKind, MirFfiOp, MirFunc, MirInst, NativeBody};
use compiler::{CompileFail, Pipeline};
use machine::native::NativeCode;
use machine::reactor::{Reactor, TestCase, TestReport};
use machine::{NativeStart, ThreadProgram};

/// What a compile leaves: the constant pool and the bodies an engine runs.
struct Compiled {
    constants: Vec<u64>,
    bodies: Vec<NativeBody>,
}

impl Compiled {
    fn body(&self, name: &str) -> &MirFunc {
        let body = self.bodies.iter().find(|b| b.name == name);
        let names = || self.bodies.iter().map(|b| b.name.as_str()).collect::<Vec<_>>();
        &body.unwrap_or_else(|| panic!("no body `{name}`; have {:?}", names())).mir
    }

    /// The instructions of `name`.
    fn insts(&self, name: &str) -> impl Iterator<Item = &MirInst> {
        self.body(name).blocks.iter().flat_map(|b| &b.insts)
    }

    /// The instructions of every body.
    fn all_insts(&self) -> impl Iterator<Item = &MirInst> {
        self.bodies.iter().flat_map(|b| &b.mir.blocks).flat_map(|b| &b.insts)
    }
}

fn allocates_object(inst: &MirInst) -> bool {
    matches!(inst, MirInst::Alloc { kind: MirAllocKind::Object { .. }, .. })
}

fn touches_field(inst: &MirInst) -> bool {
    matches!(inst, MirInst::ProductLoad { .. } | MirInst::HeapFieldStore { .. })
}

/// Run `compile` with a capture open, as a run does: without one a compile
/// leaves no bodies.
fn capturing(compile: impl FnOnce() -> Result<Vec<u64>, CompileFail>) -> Result<Compiled, CompileFail> {
    compiler::native_mir::start_native_capture();
    let constants = compile();
    let bodies = compiler::native_mir::take_native_capture();
    Ok(Compiled { constants: constants?, bodies })
}

fn compile(pipeline: &mut Pipeline, src: &str) -> Result<Compiled, CompileFail> {
    capturing(|| pipeline.compile_src(src))
}

fn compile_file(pipeline: &mut Pipeline, file: &str) -> Result<Compiled, CompileFail> {
    capturing(|| pipeline.compile_src_from_file(file))
}

/// `bodies` as the MIR interpreter runs them. A refused body fails the test:
/// it would not run.
fn interpreted(bodies: Vec<NativeBody>, pcs: &std::collections::HashMap<String, u32>) -> Arc<NativeCode> {
    let program = coil_mir_interp::prepare(bodies, pcs);
    assert!(program.refused.is_empty(), "the interpreter refused {:?}", program.refused);
    Arc::new(NativeCode::new(program.entries, program.sites).with_complete(true).with_keep(program.keep))
}

/// Function name to body entry.
fn body_ids(pipeline: &Pipeline) -> std::collections::HashMap<String, u32> {
    pipeline.program_debug_full().fn_symbols.iter().map(|s| (s.name.clone(), s.body_id)).collect()
}

fn thread_program(constants: &[u64], pipeline: &Pipeline) -> Arc<ThreadProgram> {
    Arc::new(coil_host::thread_program(
        pipeline,
        coil_host::CompiledProgram {
            constants,
            strings: pipeline.strings(),
            static_slots: pipeline.static_slot_count(),
            debug: pipeline.program_debug(),
        },
    ))
}

/// What a program did.
struct Run {
    /// Everything it printed, panic messages included.
    output: String,
    panicked: bool,
}

/// Run `program` from `main` on this thread with `code`, its output captured.
fn run_with_code(program: Arc<ThreadProgram>, code: Arc<NativeCode>, mut start: NativeStart) -> Run {
    let print = Arc::new(Mutex::new(Vec::new()));
    start.shared_print = Some(Arc::clone(&print));
    machine::native::install_shared_for_next_run(code);
    let panicked = machine::run_natively(program, start).expect("the interpreter covers every body");
    machine::io::set_shared_print_redirect(None);
    Run { output: captured_output(&print), panicked }
}

fn captured_output(print: &Mutex<Vec<u8>>) -> String {
    let bytes = print.lock().expect("print buffer mutex poisoned").clone();
    String::from_utf8(bytes).expect("captured output should be valid UTF-8")
}

/// Run `compiled` on the MIR interpreter.
fn run(compiled: Compiled, pipeline: &Pipeline, entry: Option<&Path>) -> Run {
    let code = interpreted(compiled.bodies, &body_ids(pipeline));
    let program = thread_program(&compiled.constants, pipeline);
    let entry = entry.map(coil_host::ffi_entry_path);
    let start = coil_host::native_start(pipeline, &pipeline.archived_struct_layouts(), entry.as_deref());
    run_with_code(program, code, start)
}

/// What [`run`] printed.
fn run_compiled(compiled: Compiled, pipeline: &Pipeline, entry: Option<&Path>) -> String {
    run(compiled, pipeline, entry).output
}

/// `compiled` as a `.ccoil` holds it: to bytes and back, then run from what
/// came back.
fn run_through_archive(compiled: Compiled, pipeline: &Pipeline) -> Run {
    use common::decode_archived_program;
    use compiler::native_mir::archive;
    use rkyv::rancor::Error;

    let program = pipeline.archive(compiled.constants, |_| archive::encode(&compiled.bodies, &body_ids(pipeline), &[]));
    let bytes = rkyv::to_bytes::<Error>(&program).expect("serialize");
    let loaded = decode_archived_program(bytes.as_slice()).expect("decode");
    let (bodies, pcs) = archive::decode(&loaded.native_mir).expect("decode the MIR section");
    let code = interpreted(bodies, &pcs);
    let start = coil_host::native_start(pipeline, &loaded.struct_layouts, None);
    let program = ThreadProgram {
        startup: Arc::new(loaded.startup.clone()),
        main: loaded.main,
        constants: Arc::new(loaded.constants.clone()),
        strings: Arc::new(loaded.strings.clone()),
        static_slot_count: loaded.static_slot_count,
        debug: loaded.debug_bundle(),
        class_word_kinds: machine::class_kind_table(&loaded.class_word_kinds),
        static_word_kinds: Arc::new(loaded.static_word_kinds.clone()),
    };
    run_with_code(Arc::new(program), code, start)
}

/// Run `case` as `coil test` runs one: alone, on a VM of its own, on this
/// thread, a panic's message in `print`.
fn run_case(
    pipeline: &Pipeline,
    program: &Arc<ThreadProgram>,
    code: &Arc<NativeCode>,
    reactor: &Arc<Reactor>,
    case: TestCase,
    entry: Option<&Path>,
    print: Arc<Mutex<Vec<u8>>>,
) -> TestReport {
    let start = coil_host::native_start(pipeline, &pipeline.archived_struct_layouts(), entry);
    let ctx = machine::spawn_context(Arc::clone(program), start, Arc::clone(reactor), Some(Arc::clone(code)))
        .expect("thread spawn context");
    reactor.run_test_here(ctx, case, print)
}

fn test_case(entry: u32, expect_ok_result: bool) -> TestCase {
    TestCase { entry, expect_ok_result, coverage: false, probes: 0, step_budget: None }
}

/// Run `main` as a case on a reactor of its own: what it did and the jobs it
/// submitted. Where [`run`] leaves a panic's message on stderr, this keeps it
/// in the output.
fn run_as_case(compiled: Compiled, pipeline: &Pipeline, entry: Option<&Path>) -> (Run, usize) {
    let main = pipeline.main_offset().expect("main");
    let code = interpreted(compiled.bodies, &body_ids(pipeline));
    let program = thread_program(&compiled.constants, pipeline);
    let reactor = Reactor::new(1);
    let print = Arc::new(Mutex::new(Vec::new()));
    let report = run_case(pipeline, &program, &code, &reactor, test_case(main, false), entry, Arc::clone(&print));
    let jobs = reactor.jobs_submitted();
    reactor.shutdown();
    (Run { output: captured_output(&print), panicked: !report.passed }, jobs)
}

/// Run each of `cases` alone: its name and whether it passed.
fn run_each_case<'a>(compiled: Compiled, pipeline: &Pipeline, cases: &'a [(String, u32)]) -> Vec<(&'a str, bool)> {
    let code = interpreted(compiled.bodies, &body_ids(pipeline));
    let program = thread_program(&compiled.constants, pipeline);
    let reactor = Reactor::new(1);
    let results = cases
        .iter()
        .map(|(name, entry)| {
            let print = Arc::new(Mutex::new(Vec::new()));
            let report = run_case(pipeline, &program, &code, &reactor, test_case(*entry, true), None, print);
            (name.as_str(), report.passed)
        })
        .collect();
    reactor.shutdown();
    results
}

/// Match `RUST_MIN_STACK` in `.cargo/config.toml` / the OS default main
/// thread stack (`ulimit -s`, 8 MiB on Linux/macOS) that the `coil` CLI
/// actually runs with. infer_inner/do_compile no longer have oversized
/// inline match arms (see docs/internals/limitations.md), so this is
/// headroom rather than a load-bearing requirement.
const EXAMPLE_STACK: usize = 8 * 1024 * 1024;

/// Cap concurrent 8 MiB example-runner threads. Cargo's test threads plus
/// per-VM reactor workers used to explode OS-thread churn (`make_handler`
/// SIGSEGV / hang, COI-88). Reuse a tiny pool instead of spawn/join per test.
const EXAMPLE_POOL_SIZE: usize = 2;

struct ExampleJob {
    name: String,
    work: Box<dyn FnOnce() -> String + Send>,
    reply: Sender<std::thread::Result<String>>,
}

fn example_job_tx() -> &'static Sender<ExampleJob> {
    static TX: OnceLock<Sender<ExampleJob>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<ExampleJob>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..EXAMPLE_POOL_SIZE {
            let rx = Arc::clone(&rx);
            std::thread::Builder::new()
                .name(format!("pipe-ex-{i}"))
                .stack_size(EXAMPLE_STACK)
                .spawn(move || example_pool_worker(rx))
                .expect("pipeline example pool worker");
        }
        tx
    })
}

fn example_pool_worker(rx: Arc<Mutex<Receiver<ExampleJob>>>) {
    loop {
        let job = {
            let guard = rx.lock().unwrap_or_else(|e| e.into_inner());
            guard.recv()
        };
        let Ok(job) = job else { break };
        set_worker_thread_name(&job.name);
        let result = catch_unwind(AssertUnwindSafe(|| (job.work)()));
        let _ = job.reply.send(result);
        set_worker_thread_name("pipe-ex");
    }
}

/// Linux/macOS pthread name is what `gdb` / core dumps show (15 bytes).
fn set_worker_thread_name(name: &str) {
    let mut buf = [0u8; 16];
    let bytes = name.as_bytes();
    let n = bytes.len().min(15);
    buf[..n].copy_from_slice(&bytes[..n]);
    #[cfg(target_os = "linux")]
    unsafe {
        let _ = libc::pthread_setname_np(libc::pthread_self(), buf.as_ptr().cast());
    }
    #[cfg(target_os = "macos")]
    unsafe {
        let _ = libc::pthread_setname_np(buf.as_ptr().cast());
    }
}

fn run_on_example_stack(name: String, work: impl FnOnce() -> String + Send + 'static) -> String {
    let (reply_tx, reply_rx) = mpsc::channel();
    example_job_tx()
        .send(ExampleJob {
            name,
            work: Box::new(work),
            reply: reply_tx,
        })
        .expect("pipeline example pool");
    match reply_rx.recv().expect("pipeline example pool worker") {
        Ok(output) => output,
        Err(payload) => resume_unwind(payload),
    }
}

#[test]
fn example_pool_resumes_panic_to_caller() {
    // Without resume_unwind, catch_unwind around run_example would miss
    // harness panics (FFI soft-skip probes, assertion failures).
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        run_on_example_stack("panic-probe".into(), || panic!("pool-panic-probe"));
    }));
    assert!(
        panicked.is_err(),
        "example pool must re-raise job panics to the caller"
    );
}

#[test]
fn example_pool_worker_survives_panic_for_next_job() {
    // catch_unwind in the worker is load-bearing: a killed worker leaves
    // later run_example calls blocked forever on reply_rx.recv().
    let _ = catch_unwind(AssertUnwindSafe(|| {
        run_on_example_stack("panic-then-ok".into(), || panic!("transient pool panic"));
    }));
    let out = run_on_example_stack("after-panic".into(), || "ok".to_string());
    assert_eq!(out, "ok");
}

#[test]
fn example_pool_caps_concurrent_jobs() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Barrier, Condvar};
    use std::time::{Duration, Instant};

    // Encode COI-88: peak concurrent example runners must stay ≤ pool size
    // even when many cargo-test threads submit jobs at once.
    let gate = Arc::new((Mutex::new(0usize), Condvar::new()));
    let max_seen = Arc::new(AtomicUsize::new(0));
    let n_jobs = EXAMPLE_POOL_SIZE + 4;
    let start = Arc::new(Barrier::new(n_jobs));
    let mut handles = Vec::with_capacity(n_jobs);
    for i in 0..n_jobs {
        let gate = Arc::clone(&gate);
        let max_seen = Arc::clone(&max_seen);
        let start = Arc::clone(&start);
        handles.push(std::thread::spawn(move || {
            start.wait();
            run_on_example_stack(format!("cap-{i}"), move || {
                let (lock, cvar) = &*gate;
                let mut count = lock.lock().unwrap_or_else(|e| e.into_inner());
                *count += 1;
                max_seen.fetch_max(*count, Ordering::SeqCst);
                let deadline = Instant::now() + Duration::from_millis(40);
                while Instant::now() < deadline {
                    let (guard, _) = cvar
                        .wait_timeout(count, Duration::from_millis(5))
                        .unwrap_or_else(|e| e.into_inner());
                    count = guard;
                }
                *count -= 1;
                cvar.notify_all();
                "done".into()
            })
        }));
    }
    for h in handles {
        assert_eq!(h.join().expect("submitter join"), "done");
    }
    let peak = max_seen.load(Ordering::SeqCst);
    assert!(
        (1..=EXAMPLE_POOL_SIZE).contains(&peak),
        "peak concurrent example jobs was {peak}, want 1..={EXAMPLE_POOL_SIZE}"
    );
}

fn run_example(path: &str) -> String {
    // File-backed examples may `use` stdlib modules (`io::sync`, …); in-memory
    // compile of the text alone used to miss those until `compile_src` gained
    // discovery — prefer the multifile path so entry paths/debug stay accurate.
    let path = path.to_string();
    let name = path.clone();
    run_on_example_stack(name, move || run_example_multifile(&path))
}

/// Compile and run in-memory source.
fn run_example_src(src: &str) -> String {
    run_example_src_with_entry(src, None)
}

fn assert_compile_fails(src: &str, code: compiler::ErrorCode) {
    let mut pipeline = test_pipeline();
    assert_compile_fails_pipeline(&mut pipeline, src, code);
}

/// #618: a module the checker rejects is not code-generated, so each
/// mistake is reported once (codegen used to restate them: a rejected
/// `const` assignment also gave "Undefined variable", an unknown function
/// was reported twice).
#[test]
fn rejected_module_reports_each_mistake_once() {
    for (src, code) in [
        (
            "fn main() {\n    const x = 1;\n    x = 2;\n}\n",
            compiler::ErrorCode::InvalidAssignment,
        ),
        (
            "fn main() {\n    missing_fn();\n}\n",
            compiler::ErrorCode::UnknownFunction,
        ),
        (
            "fn main() {\n    let _ = not_defined + 1;\n}\n",
            compiler::ErrorCode::UnknownValue,
        ),
    ] {
        let mut pipeline = test_pipeline();
        assert!(pipeline.compile_src(src).is_err(), "{src}");
        let errors: Vec<_> = pipeline
            .messages()
            .iter()
            .filter(|m| *m.kind() == reporting::MessageKind::ERROR)
            .collect();
        assert_eq!(errors.len(), 1, "{src}: {errors:?}");
        assert_eq!(errors[0].code(), Some(code), "{src}: {errors:?}");
    }
}

#[test]
fn fixed_array_push_is_type_error_not_mir_refuse() {
    let src = r#"
fn main() {
    let xs = [1, 2, 3];
    xs.push(4);
}
"#;
    assert_compile_fails(src, compiler::ErrorCode::FixedArrayGrow);
    let mut pipeline = test_pipeline();
    let _ = pipeline.compile_src(src);
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| m.message().contains("cannot grow")
                && m.help().as_deref() == Some("use `Vec<T>` for growable storage")),
        "Q3 front-end diagnostic, got {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| (m.code(), m.message(), m.help().clone()))
            .collect::<Vec<_>>()
    );
}

fn assert_compile_fails_pipeline(pipeline: &mut Pipeline, src: &str, code: compiler::ErrorCode) {
    let result = pipeline.compile_src(src);
    let msgs: Vec<_> = pipeline
        .messages()
        .iter()
        .map(|m| (m.code(), m.message().to_string()))
        .collect();
    assert!(
        result.is_err(),
        "expected compile failure, got Ok; {msgs:?}"
    );
    assert!(
        pipeline.messages().iter().any(|m| m.code() == Some(code)),
        "expected {code:?}, got {msgs:?}"
    );
}

fn compile_ok(pipeline: &mut Pipeline, src: &str) -> Compiled {
    compile(pipeline, src).unwrap_or_else(|_| {
        panic!(
            "expected compile Ok, messages={:?}",
            pipeline
                .messages()
                .iter()
                .map(|m| (m.code(), m.message().to_string()))
                .collect::<Vec<_>>()
        )
    })
}

/// `env::exec` moved to the coil-process package: the `env` module no
/// longer exports it, whatever is granted.
#[test]
fn env_exec_is_gone() {
    let mut pipeline = test_pipeline();
    pipeline.grant_all();
    assert!(pipeline
        .compile_src("use env::{exec};\nfn main() {\n    let args: Vec<string> = [];\n    let _ = exec(\"true\", args);\n}\n")
        .is_err());
}

#[test]
fn granted_exit_emits_host_invoke() {
    let mut pipeline = test_pipeline();
    pipeline.grant_exit();
    let compiled = compile_ok(
        &mut pipeline,
        r#"
use env::{exit};
fn main() { exit(0); }
"#,
    );
    assert!(
        compiled.all_insts().any(|i| matches!(i, MirInst::HostInvoke { .. })),
        "granted env::exit must emit HostInvoke"
    );
}

#[test]
fn granted_dload_emits_ffi_load() {
    let mut pipeline = test_pipeline();
    pipeline.grant_dload_allow("plugin");
    let compiled = compile_ok(
        &mut pipeline,
        r#"
use ffi::{dload};
fn main() { let _ = dload("plugin"); }
"#,
    );
    assert!(
        compiled.all_insts().any(|i| matches!(i, MirInst::Ffi { op: MirFfiOp::Load, .. })),
        "granted dload must emit an FFI load"
    );
}

#[test]
fn granted_ffi_exec_extern_compiles() {
    let mut pipeline = test_pipeline();
    pipeline.grant_dload_allow("plugin");
    pipeline.grant_ffi_exec();
    let _ = compile_ok(
        &mut pipeline,
        r#"
extern "plugin" {
    fn system() -> int;
}
fn main() { let _ = system(); }
"#,
    );
}

#[test]
fn host_dload_stem_grant_compiles_without_allow_dload() {
    let mut pipeline = test_pipeline();
    pipeline.grant_dload_stem("plugin");
    let _ = compile_ok(
        &mut pipeline,
        r#"
use ffi::{dload};
fn main() { let _ = dload("plugin"); }
"#,
    );
}

/// A pipeline granted every capability: these tests are about other
/// things. [`deny_pipeline`] grants none.
fn test_pipeline() -> Pipeline {
    let mut p = deny_pipeline();
    p.grant_all();
    p
}

/// [`test_pipeline`] at `-g0`: a panic prints its message alone.
fn message_only_pipeline() -> Pipeline {
    let mut p = test_pipeline();
    p.set_debug_level(compiler::DebugLevel::None);
    p
}

fn deny_pipeline() -> Pipeline {
    comptime::install();
    let mut p = Pipeline::new();
    p.bind_workspace_language_roots();
    p
}

fn compile_src_with_tests(src: &str) -> (Pipeline, Compiled) {
    let mut pipeline = test_pipeline();
    pipeline.set_include_tests(true);
    let compiled = compile(&mut pipeline, src).expect("source with tests should compile");
    (pipeline, compiled)
}

fn run_harness_src(src: &str) -> String {
    let (pipeline, compiled) = compile_src_with_tests(src);
    run_compiled(compiled, &pipeline, None)
}

fn run_example_src_with_entry(src: &str, entry: Option<&std::path::Path>) -> String {
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("example failed to compile (parse error or type errors)");
    run_compiled(compiled, &pipeline, entry)
}

/// Multi-file examples (`use` / `mod`) must go through
/// `compile_src_from_file` so the pipeline discovers dependencies
/// via bound `--root` / default `src`. In-memory `compile_src` cannot load them.
fn run_example_multifile(path: &str) -> String {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate must have a parent (workspace root)");
    let full = workspace_root.join(path);
    let mut pipeline = test_pipeline();
    let compiled = compile_file(&mut pipeline, full.to_str().unwrap()).unwrap_or_else(|_| panic!("multi-file example failed to compile: {}", full.display()));
    run_compiled(compiled, &pipeline, Some(full.as_path()))
}

/// Every `examples/*.coil` ends its header with `// @output <exact stdout>`
/// (escaped) or `// @output (not checked: <why>)`. Run each one and compare,
/// so an example cannot break or drift from what it documents (#614).
#[test]
fn examples_print_their_documented_output() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let mut names: Vec<String> = std::fs::read_dir(workspace_root.join("examples"))
        .expect("read examples/")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".coil"))
        .collect();
    names.sort();
    let mut failures = Vec::new();
    let mut checked = 0;
    for name in &names {
        let path = format!("examples/{name}");
        let src = std::fs::read_to_string(workspace_root.join(&path)).expect("read example");
        let Some(documented) = parser::test_headers::value(&src, "output") else {
            failures.push(format!("{path}: no `// @output` line in its header"));
            continue;
        };
        if documented.starts_with("(not checked") {
            continue;
        }
        // It reads `/etc/hosts` and `/etc/passwd`.
        if cfg!(windows) && name == "task_files.coil" {
            continue;
        }
        let expected = parser::test_headers::unescape(documented);
        let got = run_example(&path);
        checked += 1;
        if got != expected {
            failures.push(format!("{path}: documented {expected:?}, printed {got:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "examples disagree with their `// @output` headers:\n{}",
        failures.join("\n")
    );
    assert!(checked > 100, "only {checked} examples were checked");
}

#[cfg(unix)]
/// Soft-skip an FFI-dependent test outside CI. In CI (`CI` env set), skip is a
/// hard failure so missing `cc` / libffi never silently greens the suite.
fn ffi_soft_skip(reason: &str) {
    if std::env::var_os("CI").is_some() {
        panic!("FFI soft-skip forbidden in CI: {reason}");
    }
    eprintln!("skipping: {reason}");
}

fn run_src_with_grants(
    src: &str,
    entry: Option<&std::path::Path>,
    grants: &[(&str, std::path::PathBuf)],
) -> String {
    let mut pipeline = test_pipeline();
    for (stem, path) in grants {
        pipeline.grant_dload_file((*stem).to_string(), path.clone());
    }
    let compiled = compile(&mut pipeline, src).expect("example failed to compile (parse error or type errors)");
    run_compiled(compiled, &pipeline, entry)
}

#[test]
fn example_panic_loc_archive_has_source_files() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let path = workspace_root.join("examples/panic_loc.coil");
    let mut pipeline = test_pipeline();
    let compiled = compile_file(&mut pipeline, path.to_str().unwrap()).expect("panic_loc should compile");
    let debug = pipeline.program_debug();
    assert!(
        debug
            .source_files
            .iter()
            .any(|p| p.contains("panic_loc.coil")),
        "expected panic_loc.coil in source_files: {:?}",
        debug.source_files
    );
    assert!(
        compiled.all_insts().any(|i| matches!(i, MirInst::Panic { .. })),
        "expected a panic"
    );
}

#[test]
fn scalar_enum_construct_emits_no_make_enum() {
    let src = r#"
enum HttpCode { Ok = 200, NotFound = 404 }
fn main() {
    let s = HttpCode::Ok;
    let n = match s {
        HttpCode::Ok => s,
        HttpCode::NotFound => 0,
    };
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("scalar enum should compile");
    assert!(
        !compiled
            .all_insts()
            .any(|i| matches!(i, MirInst::Alloc { kind: MirAllocKind::Enum { .. }, .. })),
        "scalar-backed HttpCode::Ok must not allocate an enum object"
    );
}

#[test]
fn panic_aborts_and_writes_message() {
    let mut pipeline = message_only_pipeline();
    let compiled = compile(
        &mut pipeline,
            r#"
fn main() {
    panic "boom";
}
"#,
    )
    .expect("panic example should compile");
    let (run, _) = run_as_case(compiled, &pipeline, None);
    assert!(run.panicked, "expected language-level panic");
    assert_eq!(run.output, "panic: boom");
}

#[test]
fn raise_from_main_fails_the_program() {
    let mut pipeline = message_only_pipeline();
    let compiled = compile(
        &mut pipeline,
            r#"
fn main() {
    raise "boom";
}
"#,
    )
    .expect("raise in main should compile (Result mode)");
    let (run, _) = run_as_case(compiled, &pipeline, None);
    // An `Err` that ends `main` is not silent (#1353).
    assert!(run.panicked, "an Err returned from main fails the program");
    assert_eq!(run.output, "panic: main returned Err(boom)");
}













/// How many dictionaries the program's one `PolyFnCapture` holds, past
/// the shared entry's `kinds` word it captures last (#2506).
fn polyfn_capture_elems(compiled: &Compiled) -> usize {
    let capture = compiled.all_insts().find_map(|i| match i {
        MirInst::Alloc { kind: MirAllocKind::PolyFnCapture { .. }, elems, .. } => Some(elems.len() - 1),
        _ => None,
    });
    capture.expect("expected a PolyFn capture when the function escapes a constrained scope")
}

/// Phase 4: captured dictionaries remain valid after the creating frame returns
/// and the application site need not supply dictionaries (`app_dict_arity=0`).
#[test]
fn polyfn_captured_dict_survives_return() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        trait Describable<T> {
            fn describe_val(T x) -> int;
        }
        impl Describable for int {
            pub fn describe_val(int x) -> int { return x + 1; }
        }
        fn show<T: Describable>(T x) -> int {
            return x.describe_val();
        }
        fn capture_show<T: Describable>(T _w) {
            return show;
        }
        fn main() {
            let f = capture_show(0);
            write(stdout(), to_bytes(format("%i", f(41))));
        }
    "#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("compile");
    assert_eq!(
        polyfn_capture_elems(&compiled),
        1,
        "capture should reserve one Describable dict slot"
    );
    // Application of the returned PolyFn must not require a second dict at
    // the call site: evidence lives in the capture.
    assert!(
        compiled
            .all_insts()
            .any(|i| matches!(i, MirInst::CallIndirect { dicts: 0, .. })),
        "captured PolyFn call should take no dictionaries"
    );
    assert_eq!(run_example_src(src), "42");
}


/// Phase 4: multiparam `Convert<A,B>` capture works after return with no app dict.
/// Both type args are witnessed at the capture call so the dict is concrete.
#[test]
fn polyfn_multiparam_capture_survives_return() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        trait Convert<A, B> {
            fn cast(A x) -> B;
        }
        impl Convert<int> for int {
            pub fn cast(int x) -> int { return x; }
        }
        fn convert_fn<A, B>(A x) -> B where Convert<A, B> {
            return x.cast();
        }
        fn capture_convert<A, B>(A _wa, B _wb) where Convert<A, B> {
            return convert_fn;
        }
        fn main() {
            let f = capture_convert(0, 0);
            write(stdout(), to_bytes(format("%i", f(42))));
        }
    "#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("compile");
    assert_eq!(polyfn_capture_elems(&compiled), 1);
    assert_eq!(run_example_src(src), "42");
}

/// A generic function's value next to a recursive function.
#[test]
fn polyfn_with_fib_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        fn id<T>(T x) -> T { return x; }
        fn fib(int n) -> int {
            if n <= 2 { return 1; }
            return fib(n - 1) + fib(n - 2);
        }
        fn main() {
            let f = id;
            write(stdout(), to_bytes(format("%i", f(fib(6)))));
        }
    "#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("compile");
    assert!(
        compiled
            .all_insts()
            .any(|i| matches!(i, MirInst::Alloc { kind: MirAllocKind::PolyFn { .. } | MirAllocKind::PolyFnCapture { .. }, .. }))
    );
    let output = run_compiled(compiled, &pipeline, None);
    // fib(6) = 8
    assert_eq!(output, "8");
}



#[test]
fn compile_test_emits_nothing_when_checker_rejects() {
    let mut pipeline = compiler::Pipeline::new();
    pipeline.bind_workspace_language_roots();
    let src = r#"
fn main() {
    let x: int = "nope";
}
"#;
    let parser = parser::Pratt::default();
    let mut ast = parser.parse(src).expect("ill-typed program should parse");
    let compiled = capturing(|| Ok(pipeline.compile_test("", &mut ast))).expect("compile_test");
    assert!(
        compiled.bodies.is_empty(),
        "compile_test must not emit bodies for a program the checker rejected"
    );
    assert!(compiled.constants.is_empty());
}















#[cfg(unix)]
fn ensure_ffi_libsum_built() -> std::path::PathBuf {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate must have a parent (workspace root)");
    let sum_c = workspace_root.join("examples/sum.c");
    let lib_name = machine::platform_shared_lib_filename("sum");
    let libsum = workspace_root.join("examples").join(&lib_name);

    // Always rebuild if the source is newer than the shared lib, or
    // if the shared lib doesn't exist.
    let needs_build = match (sum_c.metadata(), libsum.metadata()) {
        (Ok(src_meta), Ok(so_meta)) => src_meta.modified().ok() > so_meta.modified().ok(),
        (Ok(_), Err(_)) => true,
        _ => false,
    };
    if !needs_build && libsum.exists() {
        return libsum;
    }

    let mut cmd = std::process::Command::new("cc");
    #[cfg(target_os = "macos")]
    {
        cmd.arg("-dynamiclib");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        cmd.arg("-shared").arg("-fPIC");
    }
    #[cfg(target_os = "windows")]
    {
        cmd.arg("-shared");
    }
    let status = cmd.arg("-O2").arg("-o").arg(&libsum).arg(&sum_c).status();
    match status {
        Ok(s) if s.success() => {
            if let Ok(meta) = std::fs::metadata(&libsum)
                && meta.len() < 256
            {
                eprintln!(
                    "warning: {} looks truncated ({} bytes) after cc build",
                    libsum.display(),
                    meta.len()
                );
            }
        }
        Ok(s) => {
            ffi_soft_skip(&format!(
                "FFI tests: cc returned non-zero status {}",
                s.code().unwrap_or(-1)
            ));
        }
        Err(e) => {
            ffi_soft_skip(&format!("FFI tests: failed to invoke cc: {e}"));
        }
    }
    libsum
}

#[cfg(unix)]
#[test]
fn example_ffi_sum_via_dlopen_prints_42() {
    let libsum = ensure_ffi_libsum_built();
    if !libsum.exists() {
        ffi_soft_skip(&format!("{} not built (no C compiler?)", libsum.display()));
        return;
    }

    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate must have a parent (workspace root)");

    // Absolute dload path avoids cwd races in parallel tests.
    let full = workspace_root.join("examples/ffi_sum.coil");
    let lib_abs = libsum.canonicalize().unwrap_or_else(|_| libsum.clone());
    let mut src = std::fs::read_to_string(&full)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", full.display(), e));
    src = src.replace(
        "dload(\"sum\")",
        &format!("dload(\"{}\")", lib_abs.display()),
    );

    let result = std::panic::catch_unwind(|| {
        run_src_with_grants(&src, Some(full.as_path()), &[("sum", lib_abs.clone())])
    });
    let output = match result {
        Ok(s) => s,
        Err(_) => {
            ffi_soft_skip("FFI test panicked (dlopen failure?)");
            return;
        }
    };
    assert_eq!(output, "42", "sum(40, 2) via userland FFI should print 42");
}
/// `extern "c"` (a libc alias) is denied even when `c` is granted.
#[test]
fn extern_c_is_denied_even_when_granted() {
    let mut pipeline = test_pipeline();
    pipeline.grant_dload_stem("c");
    let result = pipeline.compile_src(
        r#"
extern "c" {
    fn strlen(string s) -> int;
}
fn main() {
    let _ = strlen("hello");
}
"#,
    );
    assert!(result.is_err());
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::HostDloadDenied)),
        "expected HostDloadDenied, got {:?}",
        pipeline.messages()
    );
}

#[test]
fn userland_dload_missing_library_returns_err() {
    let output = run_userland_dload_project(
        "missing_time_trusted",
        &dload_kind_program(&missing_abs_dload("time")),
        DloadFlags { allow: &["time"], trusted: &["time"], ..Default::default() },
    );
    assert_eq!(
        output, "missing",
        "allow+trusted `time` must not be denied; got {output:?}"
    );
}

#[test]
fn userland_dload_extra_stem_hash_mismatch_is_denied() {
    let dir = std::env::temp_dir().join("coil_userland_dload_mismatch");
    let _ = std::fs::create_dir_all(&dir);
    let name = machine::platform_shared_lib_filename("plugin");
    let path = dir.join(&name);
    std::fs::write(&path, b"plugin-bytes").unwrap();
    let other = dir.join("other.bin");
    std::fs::write(&other, b"other-bytes").unwrap();
    let abs = path.to_str().unwrap().replace('\\', "/");
    let src = format!(
        r#"
use ffi::{{dload, ErrorKind}};
use io::{{stdout, write}};
use string::{{format, to_bytes}};
fn main() {{
    let r = dload("{abs}");
    let msg = match r {{
        Result::Ok(_) => "ok",
        Result::Err(e) => match e.kind {{
            ErrorKind::LibraryNotFound => "missing",
            ErrorKind::Other => "denied",
            default => "other",
        }},
    }};
    write(stdout(), to_bytes(format("%s", msg)));
}}
"#
    );
    let output = run_src_with_grants(&src, None, &[("plugin", other)]);
    assert_eq!(output, "denied");
    let _ = std::fs::remove_dir_all(&dir);
}

fn missing_abs_dload(stem: &str) -> String {
    let name = machine::platform_shared_lib_filename(stem);
    if cfg!(windows) {
        format!("C:/coil-dload-missing/{name}")
    } else {
        format!("/coil-dload-missing/{name}")
    }
}

fn dload_kind_program(path: &str) -> String {
    format!(
        r#"
use ffi::{{dload, ErrorKind}};
use io::{{stdout, write}};
use string::{{format, to_bytes}};
fn main() {{
    let r = dload("{path}");
    let msg = match r {{
        Result::Ok(_) => "ok",
        Result::Err(e) => match e.kind {{
            ErrorKind::LibraryNotFound => "missing",
            ErrorKind::Other => "denied",
            default => "other",
        }},
    }};
    write(stdout(), to_bytes(format("%s", msg)));
}}
"#
    )
}

/// `--dload-pin` / `--dload-trusted` / `--allow-dload` for a dload test.
#[derive(Default)]
struct DloadFlags<'a> {
    allow: &'a [&'a str],
    trusted: &'a [&'a str],
    pins: &'a [(&'a str, &'a str)],
}

impl DloadFlags<'_> {
    fn grants(&self, mut grants: compiler::HostGrants) -> compiler::HostGrants {
        for stem in self.allow {
            grants.grant_dload_allow(*stem);
        }
        for stem in self.trusted {
            grants.add_dload_trusted(*stem);
        }
        for (stem, sha) in self.pins {
            grants.add_dload_pin(*stem, *sha);
        }
        grants
    }
}

/// A wrong pin: no library hashes to all `a`s.
const WRONG_PIN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A fresh project directory holding `main.coil`.
fn dload_project(test_name: &str, src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("coil_dload_{test_name}_{pid}_{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir dload project");
    let entry = dir.join("main.coil");
    std::fs::write(&entry, src).expect("write main.coil");
    (dir, entry)
}

fn compiled_dload_project(
    test_name: &str,
    src: &str,
    grants: compiler::HostGrants,
) -> (Pipeline, std::path::PathBuf, std::path::PathBuf, Compiled) {
    let (dir, entry) = dload_project(test_name, src);
    let mut pipeline = Pipeline::new();
    pipeline.bind_project_roots_with_default(dir.clone(), [workspace_stdlib()]);
    pipeline.set_host_grants(grants);
    let compiled = compile_file(&mut pipeline, entry.to_str().unwrap()).unwrap_or_else(|_| {
            for msg in pipeline.messages() {
                eprintln!("PIPELINE ERROR: {}", msg.message());
            }
            panic!("dload project failed to compile");
        });
    (pipeline, dir, entry, compiled)
}

fn dload_gate_for(test_name: &str, flags: DloadFlags) -> machine::DloadGate {
    let grants = flags.grants(compiler::HostGrants::deny_all());
    let (pipeline, dir, ..) = compiled_dload_project(test_name, "fn main() {}\n", grants);
    let gate = coil_host::pipeline_dload_gate(&pipeline);
    let _ = std::fs::remove_dir_all(&dir);
    gate
}

fn assert_library_denied(gate: &machine::DloadGate, name: &str, stem: &str) {
    match gate.check_request(name) {
        Err(machine::FfiError::LibraryDenied { stem: got, .. }) => {
            assert_eq!(got, stem, "denied stem for {name}");
        }
        other => panic!("expected LibraryDenied for {name}, got {other:?}"),
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_default()
}

fn run_userland_dload_project(test_name: &str, src: &str, flags: DloadFlags) -> String {
    run_userland_dload_project_grants(test_name, src, flags, compiler::HostGrants::deny_all())
}

fn run_userland_dload_project_grants(
    test_name: &str,
    src: &str,
    flags: DloadFlags,
    grants: compiler::HostGrants,
) -> String {
    let (pipeline, dir, entry, compiled) =
        compiled_dload_project(test_name, src, flags.grants(grants));
    let output = run_compiled(compiled, &pipeline, Some(entry.as_path()));
    let _ = std::fs::remove_dir_all(&dir);
    output
}

fn assert_dload_project_compile_fails(
    test_name: &str,
    src: &str,
    flags: DloadFlags,
    code: compiler::ErrorCode,
) {
    let (dir, entry) = dload_project(test_name, src);
    let mut pipeline = Pipeline::new();
    pipeline.bind_project_roots_with_default(dir.clone(), [workspace_stdlib()]);
    pipeline.set_host_grants(flags.grants(compiler::HostGrants::deny_all()));
    let result = pipeline.compile_src_from_file(entry.to_str().unwrap());
    let msgs: Vec<_> = pipeline
        .messages()
        .iter()
        .map(|m| (m.code(), m.message().to_string()))
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        result.is_err(),
        "expected compile failure, got Ok; {msgs:?}"
    );
    assert!(
        pipeline.messages().iter().any(|m| m.code() == Some(code)),
        "expected {code:?}, got {msgs:?}"
    );
}

#[test]
fn userland_dload_trusted_extra_without_pin_is_missing_not_denied() {
    let output = run_userland_dload_project(
        "trusted_no_pin",
        &dload_kind_program(&missing_abs_dload("plugin")),
        DloadFlags { allow: &["plugin"], trusted: &["plugin"], ..Default::default() },
    );
    assert_eq!(output, "missing");
}

#[test]
fn userland_dload_trusted_extra_wrong_pin_is_missing_not_denied() {
    let output = run_userland_dload_project(
        "trusted_wrong_pin",
        &dload_kind_program(&missing_abs_dload("plugin")),
        DloadFlags {
            allow: &["plugin"],
            trusted: &["plugin"],
            pins: &[("plugin", WRONG_PIN)],
        },
    );
    assert_eq!(output, "missing");
}

#[test]
fn userland_dload_untrusted_extra_without_pin_is_denied() {
    let output = run_userland_dload_project(
        "untrusted_no_pin",
        &dload_kind_program(&missing_abs_dload("plugin")),
        DloadFlags { allow: &["plugin"], ..Default::default() },
    );
    assert_eq!(output, "denied");
}

#[test]
fn userland_dload_trusted_without_allow_is_denied() {
    assert_dload_project_compile_fails(
        "trusted_no_allow",
        &dload_kind_program(&missing_abs_dload("plugin")),
        DloadFlags { trusted: &["plugin"], ..Default::default() },
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_trusted_c_is_denied() {
    assert_dload_project_compile_fails(
        "trusted_c",
        &dload_kind_program("c"),
        DloadFlags { trusted: &["c"], ..Default::default() },
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_crypto_without_allow_is_denied() {
    assert_dload_project_compile_fails(
        "crypto_trusted_no_allow",
        &dload_kind_program(&missing_abs_dload("crypto")),
        DloadFlags { trusted: &["crypto"], ..Default::default() },
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_trusted_libc_is_denied() {
    assert_dload_project_compile_fails(
        "trusted_libc",
        &dload_kind_program("libc"),
        DloadFlags { allow: &["libc"], trusted: &["libc"], ..Default::default() },
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_allowlisted_trusted_c_is_denied() {
    assert_dload_project_compile_fails(
        "trusted_allow_c",
        &dload_kind_program("c"),
        DloadFlags { allow: &["c"], trusted: &["c"], ..Default::default() },
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_crypto_allow_without_hash_or_trusted_is_denied() {
    let output = run_userland_dload_project(
        "crypto_allow_no_hash",
        &dload_kind_program(&missing_abs_dload("crypto")),
        DloadFlags { allow: &["crypto"], ..Default::default() },
    );
    assert_eq!(output, "denied");
}

#[test]
fn userland_dload_bootstrap_crypto_allow_plus_trusted_is_missing() {
    let output = run_userland_dload_project(
        "bootstrap_crypto_trusted",
        &dload_kind_program(&missing_abs_dload("crypto")),
        DloadFlags { allow: &["crypto"], trusted: &["crypto"], ..Default::default() },
    );
    assert_eq!(output, "missing");
}

#[test]
fn userland_dload_crypto_allow_plus_pin_is_missing() {
    let output = run_userland_dload_project(
        "crypto_allow_pin",
        &dload_kind_program(&missing_abs_dload("crypto")),
        DloadFlags {
            allow: &["crypto"],
            pins: &[("crypto", WRONG_PIN)],
            ..Default::default()
        },
    );
    assert_eq!(output, "missing");
}

#[test]
fn pipeline_gate_trusted_extra_skips_native_hash() {
    let gate = dload_gate_for(
        "honor_skip_hash",
        DloadFlags { allow: &["plugin"], trusted: &["plugin"], ..Default::default() },
    );
    gate.check_request("plugin")
        .expect("trusted extra stem must pass");
    assert!(!gate.hash_required("plugin"));
}

#[test]
fn pipeline_gate_untrusted_extra_requires_hash() {
    let gate = dload_gate_for(
        "untrusted_requires_hash",
        DloadFlags { allow: &["plugin"], ..Default::default() },
    );
    assert_library_denied(&gate, "plugin", "plugin");
    assert!(gate.hash_required("plugin"));
}

#[test]
fn pipeline_gate_trusted_without_allow_still_loads() {
    let gate = dload_gate_for(
        "trusted_no_allow_gate",
        DloadFlags { trusted: &["plugin"], ..Default::default() },
    );
    gate.check_request("plugin")
        .expect("trusted is integrity, not a compile-time allow re-check");
}

#[test]
fn pipeline_gate_allowlisted_trusted_c_is_library_denied() {
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        dload_gate_for(
            "allow_trusted_c",
            DloadFlags {
                allow: &["c", "plugin"],
                trusted: &["c", "plugin"],
                ..Default::default()
            },
        )
    }));
    match panicked {
        Ok(gate) => {
            assert_library_denied(&gate, "c", "c");
            gate.check_request("plugin")
                .expect("trusted extra plugin must still pass");
        }
        Err(payload) => {
            let msg = panic_message(&payload);
            assert!(
                msg.contains("libc alias") && msg.contains("`c`"),
                "allow-listed c must not grant dload; got panic {msg:?}"
            );
        }
    }
}

#[test]
fn pipeline_gate_first_party_without_allow_is_denied() {
    let gate = dload_gate_for(
        "first_party_no_allow",
        DloadFlags { allow: &["plugin"], trusted: &["plugin"], ..Default::default() },
    );
    for stem in machine::DLOAD_PRODUCTION_STEMS {
        assert_library_denied(&gate, stem, stem);
        assert!(
            gate.hash_required(stem),
            "{stem} must require hash unless trusted"
        );
    }
}

#[test]
fn userland_dload_first_party_trusted_without_allow_is_denied() {
    for stem in machine::DLOAD_PRODUCTION_STEMS {
        assert_dload_project_compile_fails(
            &format!("{stem}_trusted_no_allow"),
            &dload_kind_program(&missing_abs_dload(stem)),
            DloadFlags { trusted: &[stem], ..Default::default() },
            compiler::ErrorCode::HostDloadDenied,
        );
    }
}

#[test]
fn userland_dload_first_party_allow_plus_trusted_is_missing() {
    for stem in machine::DLOAD_PRODUCTION_STEMS {
        let output = run_userland_dload_project(
            &format!("{stem}_allow_trusted"),
            &dload_kind_program(&missing_abs_dload(stem)),
            DloadFlags { allow: &[stem], trusted: &[stem], ..Default::default() },
        );
        assert_eq!(
            output, "missing",
            "{stem} allow+trusted must skip hash; missing file is LibraryNotFound"
        );
    }
}

#[test]
fn userland_dload_first_party_allow_without_hash_or_trusted_is_denied() {
    for stem in machine::DLOAD_PRODUCTION_STEMS {
        let output = run_userland_dload_project(
            &format!("{stem}_allow_untrusted"),
            &dload_kind_program(&missing_abs_dload(stem)),
            DloadFlags { allow: &[stem], ..Default::default() },
        );
        assert_eq!(
            output, "denied",
            "{stem} allow without trusted and without pin must be denied"
        );
    }
}

#[test]
fn pipeline_gate_first_party_allow_without_hash_or_trusted_is_denied() {
    for stem in machine::DLOAD_PRODUCTION_STEMS {
        let gate = dload_gate_for(
            &format!("{stem}_gate_no_hash"),
            DloadFlags { allow: &[stem], ..Default::default() },
        );
        assert_library_denied(&gate, stem, stem);
        assert!(gate.hash_required(stem), "{stem} must require a pin");
    }
}

#[test]
fn pipeline_gate_first_party_allow_plus_trusted_skips_hash() {
    for stem in machine::DLOAD_PRODUCTION_STEMS {
        let gate = dload_gate_for(
            &format!("{stem}_gate_trusted"),
            DloadFlags { allow: &[stem], trusted: &[stem], ..Default::default() },
        );
        gate.check_request(stem)
            .unwrap_or_else(|e| panic!("{stem} allow+trusted must pass, got {e:?}"));
        assert!(!gate.hash_required(stem), "{stem} trusted must skip hash");
    }
}








#[cfg(unix)]
fn run_ffi_example_with_lib(path: &str, lib_path: &std::path::Path) -> String {
    ensure_ffi_libsum_built();
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate must have a parent (workspace root)");
    let full = workspace_root.join(path);
    let lib_abs = lib_path
        .canonicalize()
        .unwrap_or_else(|_| lib_path.to_path_buf());
    let mut src = std::fs::read_to_string(&full)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", full.display(), e));
    src = src.replace(
        "dload(\"sum\")",
        &format!("dload(\"{}\")", lib_abs.display()),
    );
    run_src_with_grants(&src, Some(full.as_path()), &[("sum", lib_abs)])
}

#[cfg(unix)]
#[test]
fn example_ffi_array_sum_prints_15() {
    let libsum = ensure_ffi_libsum_built();
    if !libsum.exists() {
        ffi_soft_skip(&format!("{} not built", libsum.display()));
        return;
    }
    let output = run_ffi_example_with_lib("examples/ffi_array.coil", &libsum);
    assert_eq!(output, "15");
}

#[cfg(unix)]
#[test]
fn example_ffi_callback_prints_42() {
    let libsum = ensure_ffi_libsum_built();
    if !libsum.exists() {
        ffi_soft_skip(&format!("{} not built", libsum.display()));
        return;
    }
    let output = run_ffi_example_with_lib("examples/ffi_callback.coil", &libsum);
    assert_eq!(output, "42");
}

#[cfg(unix)]
#[test]
fn example_ffi_struct_return_prints_34() {
    let libsum = ensure_ffi_libsum_built();
    if !libsum.exists() {
        ffi_soft_skip(&format!("{} not built", libsum.display()));
        return;
    }
    let output = run_ffi_example_with_lib("examples/ffi_struct_ret.coil", &libsum);
    assert_eq!(output, "34");
}

#[cfg(unix)]
#[test]
fn example_ffi_callback_return_prints_1() {
    let libsum = ensure_ffi_libsum_built();
    if !libsum.exists() {
        ffi_soft_skip(&format!("{} not built", libsum.display()));
        return;
    }
    let output = run_ffi_example_with_lib("examples/ffi_callback_ret.coil", &libsum);
    assert_eq!(output, "1");
}

#[cfg(unix)]
#[test]
fn example_ffi_extern_prints_42() {
    let libsum = ensure_ffi_libsum_built();
    if !libsum.exists() {
        ffi_soft_skip(&format!("{} not built", libsum.display()));
        return;
    }
    let output = run_ffi_example_with_lib("examples/ffi_extern.coil", &libsum);
    assert_eq!(output, "42");
}

#[cfg(unix)]
#[test]
fn example_ffi_varargs_prints_60() {
    let libsum = ensure_ffi_libsum_built();
    if !libsum.exists() {
        ffi_soft_skip(&format!("{} not built", libsum.display()));
        return;
    }
    let output = run_ffi_example_with_lib("examples/ffi_varargs.coil", &libsum);
    assert_eq!(output, "60");
}




#[test]
fn example_perf_mandelbrot_ipa_emits_chunk_worker() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let full = workspace_root.join("examples/perf/mandelbrot_ipa.coil");
    let mut pipeline = test_pipeline();
    let compiled = compile_file(&mut pipeline, full.to_str().unwrap()).expect("mandelbrot_ipa should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_some(),
        "flat pixel scan must emit a loop-IPA chunk worker"
    );
    let output = run_compiled(compiled, &pipeline, Some(full.as_path()));
    assert_eq!(output, "625885");
}




/// Err arm in a binding match must still panic with the literal message.
#[test]
fn let_result_ok_panic_match_err_path_panics() {
    let mut pipeline = message_only_pipeline();
    let compiled = compile(
        &mut pipeline,
            r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let x = match Result::Err(1) {
        Result::Ok(s) => s,
        Result::Err(_) => panic "accept",
    };
    write(stdout(), to_bytes(format("%i", x)));
}
"#,
    )
    .expect("compile");
    let (run, _) = run_as_case(compiled, &pipeline, None);
    assert!(run.panicked, "expected language-level panic on Err");
    assert_eq!(run.output, "panic: accept");
}


/// Standalone virtual `main` for a green `test("…")` suite exits cleanly.
#[test]
fn harness_virtual_main_passes_when_all_asserts_ok() {
    let output = run_harness_src(
        r#"
test("ok") {
    if !(true) { raise "assertion failed"; }
}
"#,
    );
    assert_eq!(output, "");
}

/// Soft-fail path prints `> Test "…" failed` and aborts via Panic.
#[test]
fn harness_virtual_main_prints_failure_and_panics() {
    let mut pipeline = test_pipeline();
    pipeline.set_include_tests(true);
    let compiled = compile(
        &mut pipeline,
            r#"
test("broken") {
    if !(false) { raise "assertion failed"; }
}
"#,
    )
    .expect("compile");
    let (Run { output, panicked }, _) = run_as_case(compiled, &pipeline, None);
    assert!(panicked, "virtual main must panic when a case fails");
    assert!(
        output.contains("> Test \"broken\" failed"),
        "expected failure banner, got {output:?}"
    );
}

/// The virtual `main` (built as HIR) runs every case in order: a passing
/// case prints nothing, each `Err` case its banner, and later cases still
/// run after a failure.
#[test]
fn harness_virtual_main_reports_each_failed_case_in_order() {
    let mut pipeline = test_pipeline();
    pipeline.set_include_tests(true);
    let compiled = compile(
        &mut pipeline,
            r#"
static let runs: int = 0;

test("first fails") {
    runs = runs + 1;
    if !(false) { raise "assertion failed"; }
}
test("passes") {
    runs = runs + 1;
    if !(true) { raise "assertion failed"; }
}
test("returns Err") {
    runs = runs + 1;
    return Result::Err("nope");
}
test("all ran") {
    if !(runs == 3) { raise "assertion failed"; }
}
"#,
    )
    .expect("compile");
    let (Run { output, panicked }, _) = run_as_case(compiled, &pipeline, None);
    assert!(panicked, "virtual main must panic when a case fails");
    assert_eq!(
        output, "> Test \"first fails\" failed\n> Test \"returns Err\" failed\npanic: tests failed",
        "got {output:?}"
    );
}

/// The virtual `main` lowers straight from HIR to MIR: each case's boxed
/// `Result<(), string>` comes back as its niche word, the banner is a MIR
/// `print` and the final failure a MIR `panic`.
#[test]
fn harness_virtual_main_lowers_to_direct_mir() {
    let mut pipeline = test_pipeline();
    pipeline.set_include_tests(true);
    let compiled = compile(
        &mut pipeline,
        r#"
test("passes") {
    if !(true) { raise "unreachable"; }
}
test("fails") {
    return Result::Err("nope");
}
"#,
    )
    .expect("compile");
    assert!(compiled.insts("main").any(|i| matches!(i, MirInst::Print { .. })), "the banner is a MIR print");
    assert!(compiled.insts("main").any(|i| matches!(i, MirInst::Panic { .. })), "the failure is a MIR panic");
}

/// CLI-style isolation: each case runs alone, so a soft failure does not
/// prevent later cases from running.
#[test]
fn harness_isolated_case_continues_after_soft_fail() {
    let mut pipeline = test_pipeline();
    pipeline.set_include_tests(true);
    let compiled = compile(
        &mut pipeline,
            r#"
test("a") { if !(true) { raise "assertion failed"; } }
test("b") { if !(false) { raise "assertion failed"; } }
test("c") { if !(1 + 1 == 2) { raise "assertion failed"; } }
"#,
    )
    .expect("compile");
    let cases = pipeline.test_cases().to_vec();
    assert_eq!(cases.len(), 3);
    assert_eq!(
        cases.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["a", "b", "c"]
    );

    let results = run_each_case(compiled, &pipeline, &cases);
    assert_eq!(results, [("a", true), ("b", false), ("c", true)]);
}

/// Hard-`panic` path: each case is still isolated, as under `coil test`, so
/// a panic does not fail-fast.
#[test]
fn harness_isolated_case_continues_after_hard_panic() {
    let mut pipeline = test_pipeline();
    pipeline.set_include_tests(true);
    let compiled = compile(
        &mut pipeline,
            r#"
test("boom") { panic "x"; }
test("after") { if !(true) { raise "assertion failed"; } }
"#,
    )
    .expect("compile");
    let cases = pipeline.test_cases().to_vec();
    assert_eq!(
        cases.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["boom", "after"]
    );

    let results = run_each_case(compiled, &pipeline, &cases);
    assert_eq!(results, [("boom", false), ("after", true)]);
}














/// Disk-import rebind must not suppress explicit-capture checks for locals.
#[test]
fn disk_import_lambda_still_requires_local_capture() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
fn main() {
    let y = 1;
    let f = fn () => write_all(stdout(), to_bytes(format("%i", y)));
    f()?;
}
"#,
    );
    assert!(
        result.is_err(),
        "local `y` must still require `use (y)`: {:?}",
        pipeline.messages()
    );
    assert!(
        pipeline.messages().iter().any(|m| {
            m.message().contains("cannot capture `y` without `use (y)`")
                || m.message().contains("list `y` in the enclosing `use")
                || m.message().contains("list `y` in the lambda's `use")
        }),
        "expected capture diagnostic for `y`, got: {:?}",
        pipeline.messages()
    );
}





#[test]
fn production_compile_strips_harness_declarations() {
    let mut pipeline = test_pipeline();
    let compiled = compile(
        &mut pipeline,
            r#"
use io::{stdout, write};
use string::{format, to_bytes};
test("hidden") { if !(false) { raise "assertion failed"; } }
test("also hidden") { if !(false) { raise "assertion failed"; } }
fn main() { write(stdout(), to_bytes("ok")); }
"#,
    )
    .expect("compile");
    assert!(
        pipeline.test_cases().is_empty(),
        "production compile must not register harness cases"
    );
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "ok");
}

#[test]
fn include_tests_flag_embeds_harness_metadata() {
    let (pipeline, _compiled) = compile_src_with_tests(
        r#"
test("via attr") { if !(true) { raise "assertion failed"; } }
test("via block") { if !(true) { raise "assertion failed"; } }
"#,
    );
    assert_eq!(pipeline.test_cases().len(), 2);
    assert_eq!(pipeline.test_cases()[0].0, "via attr");
    assert_eq!(pipeline.test_cases()[1].0, "via block");
}


















/// `if !done { break }` takes the break.
#[test]
fn not_flag_break_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let done = false;
    let n = 0;
    while (n < 10) {
        if !done { break; }
        n = n + 1;
    }
    write(stdout(), to_bytes(format("%i", n)));
}
"#;
    assert_eq!(run_example_src(src), "0");
}

/// Two-local `if a < b { break }` takes the break.
#[test]
fn two_local_compare_break_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let v: Vec<int> = Vec::new();
    v.push(1);
    v.push(2);
    let a = v[0];
    let b = v[1];
    let n = 0;
    while (n < 10) {
        if a < b { break; }
        n = n + 1;
    }
    write(stdout(), to_bytes(format("%i", n)));
}
"#;
    // Heap reads, not literals: a constant compare would fold away.
    assert_eq!(run_example_src(src), "0");
}








/// A recursion whose leaves compare with a user `Lt` is not pure, so it
/// gets no fork worker: the impl's static updates would be lost on worker
/// threads (#2388).
#[test]
fn auto_par_user_operator_recursion_is_not_forked() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
static let CALLS: int = 0;
class Money {
    pub cents: int,
}
impl Lt for Money {
    pub fn lt(Money a, Money b) -> bool {
        CALLS++;
        return a.cents < b.cents;
    }
}
fn walk(int n) -> int {
    if n <= 1 {
        if new Money(n) < new Money(5) {
            return 1;
        }
        return 0;
    }
    return walk(n - 1) + walk(n - 2);
}
fn main() {
    let r = walk(27);
    write(stdout(), to_bytes(format("%i %i", r, CALLS)));
}
"#;
    let mut pipeline = test_pipeline();
    pipeline.set_call_fold(false);
    compile(&mut pipeline, src).expect("walk should compile");
    assert!(
        pipeline.function_offset("__coil_par_walk").is_none(),
        "a recursion that runs a user operator must stay sequential"
    );
}

/// Constant sites above the expression grain floor must emit `__coil_par_*` and stay correct.
#[test]
fn auto_par_fib_above_threshold_emits_spec_and_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    write(stdout(), to_bytes(format("%i", fib(22))));
}
"#;
    let mut pipeline = test_pipeline();
    // The call would be computed while compiling; this checks its worker.
    pipeline.set_call_fold(false);
    let compiled = compile(&mut pipeline, src).expect("auto-par fib should compile");
    assert!(
        pipeline.function_offset("__coil_par_fib").is_some(),
        "expected one parameterized worker for fib"
    );
    assert!(
        pipeline.function_offset("__coil_par_fib_22").is_none(),
        "must not emit a per-arg clone for fib(22)"
    );
    assert!(
        pipeline.function_offset("__coil_par_fib_21").is_none(),
        "hops are depth on the same worker, not __coil_par_fib_21"
    );
    assert!(
        pipeline.function_offset("__coil_par_fib_20").is_none(),
        "fib(20) must not get a parallel specialization"
    );
    let (Run { output, .. }, jobs) = run_as_case(compiled, &pipeline, None);
    assert_eq!(output, "17711");
    assert!(
        (1..20).contains(&jobs),
        "fib(22) must spawn, but not a cutoff-chain storm: jobs={jobs}"
    );
}

/// An `EnumCtor` combine forks both arms into a constructor, not a binop.
#[test]
fn auto_par_enum_ctor_emits_spec_and_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Tree {
    Leaf,
    Node(Tree, Tree),
}
fn build(int n) -> Tree {
    if n <= 1 {
        return Tree::Leaf();
    }
    return Tree::Node(build(n - 1), build(n - 2));
}
fn leaves(Tree t) -> int {
    return match t {
        Tree::Leaf => 1,
        Tree::Node(l, r) => leaves(l) + leaves(r),
    };
}
fn main() {
    write(stdout(), to_bytes(format("%i", leaves(build(21)))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("auto-par enum-ctor should compile");
    assert!(
        pipeline.function_offset("__coil_par_build").is_some(),
        "expected an enum-ctor worker for build"
    );
    // `build` shares fib's recurrence, so the leaf count is fib(22).
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "17711");
}

/// Impurity nested only inside an enum constructor payload must still block
/// `__coil_par_*` clones (purity used to skip Construct payloads entirely).
#[test]
fn impure_call_in_enum_ctor_payload_skips_par_specialization() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Cell {
    Val(int),
}
fn shout(int n) -> int {
    write(stdout(), to_bytes(format("%i", n)));
    return n;
}
fn rec(int n) -> int {
    if n <= 1 { return n; }
    let _ = Cell::Val(shout(n));
    return rec(n - 1) + rec(n - 2);
}
fn main() {
    write(stdout(), to_bytes(format("%i", rec(22))));
}
"#;
    let mut pipeline = test_pipeline();
    let _ = pipeline
        .compile_src(src)
        .expect("impure ctor-payload rec should still compile");
    assert!(
        pipeline.function_offset("__coil_par_rec").is_none(),
        "impurity inside a Construct payload must block auto-par"
    );
}

/// Subtraction combines are first-class IPA arms, not only Add/Mul.
#[test]
fn auto_par_sub_binop_emits_spec_and_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn diff(int n) -> int {
    if n <= 1 {
        return n;
    }
    return diff(n - 1) - diff(n - 2);
}
fn main() {
    write(stdout(), to_bytes(format("%i", diff(22))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("auto-par sub binop should compile");
    assert!(
        pipeline.function_offset("__coil_par_diff").is_some(),
        "expected a Sub worker for diff"
    );
    let output = run_compiled(compiled, &pipeline, None);
    // d(n)=d(n-1)-d(n-2) with d(0)=0,d(1)=1 is period-6; d(22)=-1.
    assert_eq!(output, "-1");
}

/// A `SelfCall` combine rebuilds the outer N-ary call from the joined arms.
///
/// `tak(24, 22, 20)` looks big but is 53 calls — the work score refuses it, so
/// this uses a load whose tree is genuinely deep.
#[test]
fn auto_par_self_call_combine_emits_spec() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn tak(int a, int b, int c) -> int {
    if b >= a {
        return c;
    }
    return tak(tak(a - 1, b, c), tak(b - 1, c, a), tak(c - 1, a, b));
}
fn main() {
    write(stdout(), to_bytes(format("%i", tak(21, 12, 6))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("auto-par self-call should compile");
    assert!(
        pipeline.function_offset("__coil_par_tak").is_some(),
        "expected a parameterized worker for tak(21, 12, 6)"
    );
    assert!(
        pipeline.function_offset("__coil_par_tak_21_12_6").is_none(),
        "must not emit a per-arg tak clone"
    );
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "12");
}

/// A narrow `x - y` gap is tiny work even with large components.
#[test]
fn auto_par_narrow_tak_stays_sequential() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn tak(int a, int b, int c) -> int {
    if b >= a {
        return c;
    }
    return tak(tak(a - 1, b, c), tak(b - 1, c, a), tak(c - 1, a, b));
}
fn main() {
    write(stdout(), to_bytes(format("%i", tak(24, 22, 20))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("narrow tak should compile");
    assert!(
        pipeline.function_offset("__coil_par_tak").is_none(),
        "a narrow x - y gap must not emit a worker"
    );
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "21");
}

/// The fair `tak(18, 12, 6)` load is below the tight fib floor but above the
/// loose `SelfCall` floor, so it emits one parameterized worker.
#[test]
fn auto_par_fair_tak_load_emits_worker() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn tak(int a, int b, int c) -> int {
    if b >= a {
        return c;
    }
    return tak(tak(a - 1, b, c), tak(b - 1, c, a), tak(c - 1, a, b));
}
fn main() {
    write(stdout(), to_bytes(format("%i", tak(18, 12, 6))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("fair tak should compile");
    assert!(
        pipeline.function_offset("__coil_par_tak").is_some(),
        "fair tak(18, 12, 6) must get a parallel worker under loose grain"
    );
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "7");
}

/// Impure recursive binops must not grow `__coil_par_*` clones.
#[test]
fn impure_recursive_binop_skips_par_specialization() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn leaf(int n) -> int {
    write(stdout(), to_bytes(format("%i", n)));
    return n;
}
fn rec(int n) -> int {
    if n <= 1 { return leaf(n); }
    return rec(n - 1) + rec(n - 2);
}
fn main() {
    write(stdout(), to_bytes(format("%i", rec(22))));
}
"#;
    let mut pipeline = test_pipeline();
    let _ = pipeline
        .compile_src(src)
        .expect("impure rec should still compile");
    assert!(
        pipeline.function_offset("__coil_par_rec").is_none(),
        "impure recursion must not emit auto-par workers"
    );
    assert!(
        pipeline.messages().iter().any(|m| {
            m.code() == Some(compiler::ErrorCode::ParLockHint)
                && m.message().contains("`stdout` (FD)")
                && m.message().contains("call bag in `rec`")
        }),
        "F3 must name the FD escape: {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

/// Unlocked mutex on a call bag: named hint, still sequential (no auto-fork).
#[test]
fn auto_par_mutex_escape_hints_and_stays_sequential() {
    let src = r#"
use thread::{mutex, with_lock};
fn bump(Mutex m) {
    with_lock(m, fn (int x) => (x + 1, 0))?;
}
fn rec(Mutex m, int n) -> int {
    if n <= 1 {
        bump(m);
        return n;
    }
    return rec(m, n - 1) + rec(m, n - 2);
}
fn main() {
    let m = mutex(0)?;
    rec(m, 3);
}
"#;
    let mut pipeline = test_pipeline();
    let _ = pipeline
        .compile_src(src)
        .expect("mutex-escape rec should compile");
    assert!(
        pipeline.function_offset("__coil_par_rec").is_none(),
        "unlocked mutex escape must not auto-fork"
    );
    assert!(
        pipeline.messages().iter().any(|m| {
            m.code() == Some(compiler::ErrorCode::ParLockHint)
                && m.message().contains("call bag in `rec`")
                && m.message().contains("`m` (mutex)")
                && !m.message().contains("covering lock")
        }),
        "F3 must name the mutex escape: {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

/// `open` bound to a local: write through that FD names the binding, still sequential.
#[test]
fn auto_par_open_fd_hint_names_binding() {
    let src = r#"
use io::{open, write};
use string::{format, to_bytes};
fn dump(int n) -> int {
    let f = match open("coil_f3_open_hint.bin", "w") {
        Result::Ok(h) => h,
        Result::Err(_) => panic "open failed",
    };
    write(f, to_bytes(format("%i", n)));
    return n;
}
fn rec(int n) -> int {
    if n <= 1 { return dump(n); }
    return rec(n - 1) + rec(n - 2);
}
fn main() {
    rec(3);
}
"#;
    let mut pipeline = test_pipeline();
    let _ = pipeline
        .compile_src(src)
        .expect("open-escape rec should compile");
    assert!(
        pipeline.function_offset("__coil_par_rec").is_none(),
        "unlocked file FD must not auto-fork"
    );
    assert!(
        pipeline.messages().iter().any(|m| {
            m.code() == Some(compiler::ErrorCode::ParLockHint)
                && m.message().contains("call bag in `rec`")
                && m.message().contains("`f` (FD)")
        }),
        "F3 must name the open-bound FD: {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

/// stderr + stdout on one bag: hint text lists both edges.
#[test]
fn auto_par_stderr_stdout_multi_edge_hint() {
    let src = r#"
use io::{stdout, stderr, write};
use string::{format, to_bytes};
fn leaf(int n) -> int {
    write(stdout(), to_bytes(format("%i", n)));
    write(stderr(), to_bytes(format("%i", n)));
    return n;
}
fn rec(int n) -> int {
    if n <= 1 { return leaf(n); }
    return rec(n - 1) + rec(n - 2);
}
fn main() {
    rec(3);
}
"#;
    let mut pipeline = test_pipeline();
    let _ = pipeline
        .compile_src(src)
        .expect("stdio multi-edge rec should compile");
    assert!(pipeline.function_offset("__coil_par_rec").is_none());
    assert!(
        pipeline.messages().iter().any(|m| {
            m.code() == Some(compiler::ErrorCode::ParLockHint)
                && m.message().contains("`stdout` (FD)")
                && m.message().contains("`stderr` (FD)")
                && m.message().contains(", ")
        }),
        "F3 must list both stdio edges: {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

/// FFI dload/invoke on a call bag: named FFI handle, still sequential.
#[test]
fn auto_par_ffi_invoke_hint_stays_sequential() {
    let src = r#"
use ffi::{declare, dload, invoke};
use ffi::types::{Int};
fn poke(int n) -> int {
    let lib = match dload("plugin") {
        Result::Ok(h) => h,
        Result::Err(e) => panic e.message,
    };
    let id = match declare(lib, "noop", (Int,), Int) {
        Result::Ok(v) => v,
        Result::Err(e) => panic e.message,
    };
    let _ = match invoke(lib, id, (n,)) {
        Result::Ok(v) => v,
        Result::Err(_) => n,
    };
    return n;
}
fn rec(int n) -> int {
    if n <= 1 { return poke(n); }
    return rec(n - 1) + rec(n - 2);
}
fn main() {
    rec(3);
}
"#;
    let mut pipeline = test_pipeline();
    pipeline.grant_dload_allow("plugin");
    let _ = pipeline
        .compile_src(src)
        .expect("ffi-escape rec should compile");
    assert!(
        pipeline.function_offset("__coil_par_rec").is_none(),
        "unlocked FFI must not auto-fork"
    );
    assert!(
        pipeline.messages().iter().any(|m| {
            m.code() == Some(compiler::ErrorCode::ParLockHint)
                && m.message().contains("call bag in `rec`")
                && m.message().contains("(FFI handle)")
        }),
        "F3 must name the FFI escape: {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

/// Independent pure helper arms (not self-recursion) still fork-join — the
/// arms' own subtrees are what the work score charges the site for.
#[test]
fn auto_par_pure_helper_arms_emits_spec_and_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn pair_fib(int n) -> int {
    if n <= 0 { return 0; }
    return fib(n) + fib(n - 1);
}
fn main() {
    write(stdout(), to_bytes(format("%i", pair_fib(22))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("helper-arm IPA should compile");
    assert!(
        pipeline.function_offset("__coil_par_pair_fib").is_some(),
        "expected a worker for pair_fib(22)"
    );
    assert!(
        pipeline.function_offset("__coil_par_fib").is_none(),
        "helper arms must stay sequential (no nested fib AlwaysPar)"
    );
    // fib(22) + fib(21)
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "28657");
}

/// Trivial helper arms are cheaper than a spawn, so they stay sequential.
#[test]
fn auto_par_trivial_helper_arms_stay_sequential() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int n) -> int {
    return n * n;
}
fn pair_sq(int n) -> int {
    if n <= 0 { return 0; }
    return sq(n) + sq(n - 1);
}
fn main() {
    write(stdout(), to_bytes(format("%i", pair_sq(22))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("trivial helper arms should compile");
    assert!(
        pipeline.function_offset("__coil_par_pair_sq").is_none(),
        "two multiplies must not buy a spawn"
    );
    // 22² + 21²
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "925");
}

/// Fork inside an irrefutable match arm keeps evaluable path guards.
#[test]
fn auto_par_irrefutable_match_arm_emits_spec() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn fibm(int n) -> int {
    if n <= 1 { return n; }
    return match n {
        default => fibm(n - 1) + fibm(n - 2),
    };
}
fn main() {
    write(stdout(), to_bytes(format("%i", fibm(22))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("match-arm IPA should compile");
    assert!(
        pipeline.function_offset("__coil_par_fibm").is_some(),
        "irrefutable match arm must still emit a worker"
    );
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "17711");
}

/// Nested associative `+` of helper arms is one N-arm site (not a binary leftover).
#[test]
fn auto_par_nary_add_emits_spec_and_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}
fn triple_fib(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return fib(n) + fib(n - 1) + fib(n - 2);
}
fn main() {
    write(stdout(), to_bytes(format("%i", triple_fib(22))));
}
"#;
    let mut pipeline = test_pipeline();
    // The call would be computed while compiling; this checks its worker.
    pipeline.set_call_fold(false);
    let compiled = compile(&mut pipeline, src).expect("auto-par triple_fib should compile");
    assert!(
        pipeline.function_offset("__coil_par_triple_fib").is_some(),
        "expected a 3-arm worker for triple_fib"
    );
    let (Run { output, .. }, jobs) = run_as_case(compiled, &pipeline, None);
    // fib(22)+fib(21)+fib(20) with fib(n<=1)=n
    assert_eq!(output, "35422");
    assert!(
        (1..20).contains(&jobs),
        "triple_fib(22) must spawn, but not a cutoff-chain storm: jobs={jobs}"
    );
}

/// Let-bound independent calls are the same arms as an in-expression combine.
#[test]
fn auto_par_let_bound_arms_emits_spec_and_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    let a = fib(n - 1);
    let b = fib(n - 2);
    return a + b;
}
fn main() {
    write(stdout(), to_bytes(format("%i", fib(22))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("let-bound IPA should compile");
    assert!(
        pipeline.function_offset("__coil_par_fib").is_some(),
        "expected a worker for let-bound fib"
    );
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "17711");
}

/// `param + k` is a structural arg form (binary tree over n).
#[test]
fn auto_par_param_plus_emits_spec_and_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn walk(int n, int k) -> int {
    if n <= 0 {
        return k;
    }
    return walk(n - 1, k) + walk(n - 1, k + 1);
}
fn main() {
    write(stdout(), to_bytes(format("%i", walk(16, 0))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("param-plus IPA should compile");
    assert!(
        pipeline.function_offset("__coil_par_walk").is_some(),
        "expected a worker for walk(16, 0)"
    );
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "524288");
}

/// Xor combines are associative IPA arms.
#[test]
fn auto_par_xor_binop_emits_spec_and_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn mix(int n) -> int {
    if n <= 1 {
        return n;
    }
    return mix(n - 1) ^ mix(n - 2);
}
fn main() {
    write(stdout(), to_bytes(format("%i", mix(22))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("auto-par xor should compile");
    assert!(
        pipeline.function_offset("__coil_par_mix").is_some(),
        "expected a Xor worker for mix"
    );
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "1");
}

/// A pure counted loop above the threshold must split into chunk workers and
/// still fold to the sequential sum.
#[test]
fn auto_par_loop_sum_splits_and_matches_sequential() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int i) -> int {
    return i * i;
}
fn main() {
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + sq(i);
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i,%i", acc, i)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("auto-par loop should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_some(),
        "expected a chunk worker for the counted loop"
    );
    // Sum of i*i for i in 0..100, and the induction variable past its range.
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "328350,100");
}

/// E1: a function that passes a pure lambda to a higher-order function is
/// pure (HIR effects), so a loop over it splits; an impure lambda keeps it
/// sequential. (`twice` is not generic: coil-lang#699.) The fold is `^`:
/// what a function value returns has no known sign, and a `+` fold of
/// such terms runs sequentially (coil-lang#2423).
#[test]
fn auto_par_loop_over_a_pure_lambda_call_splits() {
    let body = |lambda: &str| {
        format!(
            r#"
use io::{{stdout, write}};
use string::{{format, to_bytes}};
static let HITS: int = 0;
fn twice(int x, int -> int f) -> int {{
    return f(f(x));
}}
fn bump(int x) -> int {{
    HITS = HITS + 1;
    return x;
}}
fn score(int i) -> int {{
    return twice(i, {lambda});
}}
fn main() {{
    let acc = 0;
    let i = 0;
    while i < 100 {{
        acc = acc ^ score(i);
        i = i + 1;
    }}
    write(stdout(), to_bytes(format("%i", acc)));
}}
"#
        )
    };
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, &body("fn (int x) => x + 2")).expect("pure lambda loop should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_some(),
        "expected a chunk worker: score is pure through its lambda"
    );
    // The xor of i + 4 for i in 0..100.
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "0");

    let mut pipeline = test_pipeline();
    pipeline
        .compile_src(&body("fn (int x) => bump(x)"))
        .expect("impure lambda loop should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_none(),
        "a lambda that writes a static keeps the loop sequential"
    );
}

/// A parameter bound is split only when the trip count clears the grain floor.
#[test]
fn auto_par_loop_dynamic_bound_matches_sequential() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int i) -> int {
    return i * i;
}
fn run(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        acc = acc + sq(i);
        i = i + 1;
    }
    return acc;
}
fn main() {
    write(stdout(), to_bytes(format("%i,%i", run(40), run(10))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("dynamic loop should compile");
    assert!(pipeline.function_offset("__coil_par_loop_1").is_some());
    // sum of squares 0..40 is 20540; 0..10 stays under the grain floor.
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "20540,285");
}

/// An int parameter is a worker argument, not a reason to stay sequential.
/// The guard gives it a sign: a `+` fold of terms whose sign is the
/// caller's runs sequentially (coil-lang#2423).
#[test]
fn auto_par_loop_int_capture_matches_sequential() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn run(int k) -> int {
    if k < 0 {
        return 0;
    }
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + k;
        i = i + 1;
    }
    return acc;
}
fn main() {
    write(stdout(), to_bytes(format("%i", run(3))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("captured int loop should compile");
    assert!(pipeline.function_offset("__coil_par_loop_1").is_some());
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "300");
}

/// A pure branch still folds to the sequential sum.
#[test]
fn auto_par_loop_branch_matches_sequential() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let acc = 0;
    let i = 0;
    while i < 40 {
        if i % 2 == 0 {
            acc = acc + i;
        }
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i,%i", acc, i)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("branched loop should compile");
    assert!(pipeline.function_offset("__coil_par_loop_1").is_some());
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "380,40");
}

/// Constant stride partitions the same lattice the sequential loop walks.
#[test]
fn auto_par_loop_stride_matches_sequential() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let acc = 0;
    let i = 0;
    while i < 80 {
        acc = acc + i;
        i = i + 2;
    }
    write(stdout(), to_bytes(format("%i,%i", acc, i)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("stride loop should compile");
    assert!(pipeline.function_offset("__coil_par_loop_1").is_some());
    let output = run_compiled(compiled, &pipeline, None);
    // 0+2+...+78 = 1560, and the induction variable lands on 80.
    assert_eq!(output, "1560,80");
}

/// `fib(n)` on a local enters the fork worker when `n` clears the cutoff.
#[test]
fn auto_par_dynamic_fib_matches_sequential() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let n = 22;
    write(stdout(), to_bytes(format("%i", fib(n))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("dynamic fib should compile");
    assert!(pipeline.function_offset("__coil_par_fib").is_some());
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "17711");
}

/// The program calls `thread_spawn_shared`.
fn spawns_shared(compiled: &Compiled) -> bool {
    let shared_id = common::THREAD_SPAWN_SHARED_ID;
    compiled
        .all_insts()
        .any(|i| matches!(i, MirInst::HostInvoke { native_id, .. } if *native_id == shared_id))
}

/// C1 loop IPA emits `thread_spawn_shared` and skips `PortableValue` encode.
#[test]
fn auto_par_loop_uses_shared_heap_spawn() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int i) -> int {
    return i * i;
}
fn main() {
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + sq(i);
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("auto-par loop should compile");
    assert!(
        spawns_shared(&compiled),
        "loop IPA should HostInvoke thread_spawn_shared"
    );
    machine::thread::reset_portable_encode_count();
    let before = machine::thread::portable_encode_count();
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "328350");
    let encoded = machine::thread::portable_encode_count() - before;
    assert_eq!(
        encoded, 0,
        "C1 shared-heap loop spawn must not walk PortableValue (saw {encoded} encodes)"
    );
}

/// C2 expression IPA emits `thread_spawn_shared` and skips `PortableValue` encode.
#[test]
fn auto_par_fib_uses_shared_heap_spawn() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    write(stdout(), to_bytes(format("%i", fib(22))));
}
"#;
    let mut pipeline = test_pipeline();
    // The call would be computed while compiling; this checks its worker.
    pipeline.set_call_fold(false);
    let compiled = compile(&mut pipeline, src).expect("auto-par fib should compile");
    assert!(
        spawns_shared(&compiled),
        "expression IPA should HostInvoke thread_spawn_shared"
    );
    machine::thread::reset_portable_encode_count();
    let before = machine::thread::portable_encode_count();
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "17711");
    let encoded = machine::thread::portable_encode_count() - before;
    assert_eq!(
        encoded, 0,
        "C2 shared-heap fib spawn must not walk PortableValue (saw {encoded} encodes)"
    );
}

/// EnumCtor IPA publishes heap pointers on the shared Heap (no graph copy).
#[test]
fn auto_par_enum_ctor_shared_heap_skips_portable_copy() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Tree {
    Leaf,
    Node(Tree, Tree),
}
fn build(int n) -> Tree {
    if n <= 1 {
        return Tree::Leaf();
    }
    return Tree::Node(build(n - 1), build(n - 2));
}
fn leaves(Tree t) -> int {
    return match t {
        Tree::Leaf => 1,
        Tree::Node(l, r) => leaves(l) + leaves(r),
    };
}
fn main() {
    write(stdout(), to_bytes(format("%i", leaves(build(21)))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("auto-par enum-ctor should compile");
    assert!(
        spawns_shared(&compiled),
        "enum-ctor IPA should HostInvoke thread_spawn_shared"
    );
    machine::thread::reset_portable_encode_count();
    let before = machine::thread::portable_encode_count();
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "17711");
    let encoded = machine::thread::portable_encode_count() - before;
    assert_eq!(
        encoded, 0,
        "C2 shared-heap enum-ctor spawn must not walk PortableValue (saw {encoded} encodes)"
    );
}

/// User `thread::spawn` stays isolate + `PortableValue` (C0 Q4).
#[test]
fn user_thread_spawn_still_encodes_portable_args() {
    let src = r#"
use thread::{join, spawn};
use io::{stdout, write};
use string::{format, to_bytes};
fn add1(int n) -> int {
    return n + 1;
}
fn main() {
    let t = spawn(add1, 41)?;
    let v = join(t)?;
    write(stdout(), to_bytes(format("%i", v)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("user spawn should compile");
    assert!(
        !spawns_shared(&compiled),
        "user thread::spawn must not emit thread_spawn_shared"
    );
    machine::thread::reset_portable_encode_count();
    let before = machine::thread::portable_encode_count();
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "42");
    let encoded = machine::thread::portable_encode_count() - before;
    assert!(
        encoded >= 1,
        "isolate spawn must still encode PortableValue args (saw {encoded})"
    );
}

/// Two independent loops in one function each get their own chunk worker.
#[test]
fn auto_par_two_loops_emit_distinct_workers() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int i) -> int {
    return i * i;
}
fn main() {
    let a = 0;
    let i = 0;
    while i < 30 {
        a = a + sq(i);
        i = i + 1;
    }
    let b = 0;
    let j = 0;
    while j < 40 {
        b = b + j;
        j = j + 1;
    }
    write(stdout(), to_bytes(format("%i,%i", a, b)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("two auto-par loops should compile");
    for name in ["__coil_par_loop_1", "__coil_par_loop_2"] {
        assert!(
            pipeline.function_offset(name).is_some(),
            "expected a chunk worker named {name}"
        );
    }
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "8555,780");
}

/// A body write the analysis cannot prove independent keeps the loop sequential.
#[test]
fn impure_loop_body_skips_par_worker() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let acc = 0;
    let i = 0;
    while i < 100 {
        write(stdout(), to_bytes(format("%i", i)));
        acc = acc + i;
        i = i + 1;
    }
}
"#;
    let mut pipeline = test_pipeline();
    let _ = pipeline
        .compile_src(src)
        .expect("impure loop should still compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_none(),
        "an observable body must not be chunked"
    );
}

/// A trip count at or below the threshold must stay on the sequential loop.
#[test]
fn below_threshold_loop_skips_par_worker() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int i) -> int {
    return i * i;
}
fn main() {
    let acc = 0;
    let i = 0;
    while i < 20 {
        acc = acc + sq(i);
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("short loop should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_none(),
        "a 20-trip loop cannot pay for a spawn"
    );
    assert_eq!(run_compiled(compiled, &pipeline, None), "2470");
}

/// Counted `for x in 0..K` above the trip-count grain gate chunks like `while`.
#[test]
fn auto_par_for_range_sum_splits_and_matches_sequential() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int i) -> int {
    return i * i;
}
fn main() {
    let acc = 0;
    for x in 0..100 {
        acc = acc + sq(x);
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("auto-par for-range should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_some(),
        "expected a chunk worker for the counted for-range"
    );
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "328350");
}

/// Inclusive `for x in a..=b` normalizes to half-open and still folds.
#[test]
fn auto_par_for_inclusive_range_matches_sequential() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int i) -> int {
    return i * i;
}
fn main() {
    let acc = 0;
    for x in 1..=60 {
        acc = acc + sq(x);
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("inclusive for-range should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_some(),
        "expected a chunk worker for inclusive for-range"
    );
    // sum of i*i for i in 1..=60 = n(n+1)(2n+1)/6 at n=60.
    assert_eq!(run_compiled(compiled, &pipeline, None), "73810");
}

/// B5: `let r = 0..K` then `for x in r` is the same counted latch.
#[test]
fn auto_par_for_const_range_local_splits() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int i) -> int {
    return i * i;
}
fn main() {
    let r = 0..100;
    let acc = 0;
    for x in r {
        acc = acc + sq(x);
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("auto-par const range local should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_some(),
        "expected a chunk worker for a const range local"
    );
    assert_eq!(run_compiled(compiled, &pipeline, None), "328350");
}

/// XOR reduction is associative with identity 0, so counted loops still chunk.
#[test]
fn auto_par_loop_xor_splits_and_matches_sequential() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int i) -> int {
    return i * i;
}
fn main() {
    let acc = 0;
    for x in 0..100 {
        acc = acc ^ sq(x);
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("auto-par xor loop should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_some(),
        "expected a chunk worker for xor reduction"
    );
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "13452");
}

/// Enclosing const ints are worker immediates, not live captures.
#[test]
fn auto_par_loop_const_scale_splits() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int i) -> int {
    return i * i;
}
fn main() {
    let scale = 3;
    let acc = 0;
    for x in 0..100 {
        acc = acc + scale * sq(x);
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("auto-par const-scale loop should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_some(),
        "expected a chunk worker for const-int scale"
    );
    let output = run_compiled(compiled, &pipeline, None);
    assert_eq!(output, "985050");
}

/// `acc = e + acc` is the same commutative reduction as `acc = acc + e`.
#[test]
fn auto_par_loop_commuted_add_splits() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int i) -> int {
    return i * i;
}
fn main() {
    let acc = 0;
    for x in 0..100 {
        acc = sq(x) + acc;
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("commuted add loop should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_some(),
        "expected a chunk worker for commuted add"
    );
    assert_eq!(run_compiled(compiled, &pipeline, None), "328350");
}

/// Dynamic `for x in 0..n` splits when the runtime trip count clears the grain floor.
#[test]
fn dynamic_for_range_splits_and_matches_sequential() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int i) -> int {
    return i * i;
}
fn run(int n) -> int {
    let acc = 0;
    for x in 0..n {
        acc = acc + sq(x);
    }
    return acc;
}
fn main() {
    write(stdout(), to_bytes(format("%i", run(100))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("dynamic for-range should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_some(),
        "a parameter range above the grain floor should spawn"
    );
    assert_eq!(run_compiled(compiled, &pipeline, None), "328350");
}

/// A for-range at the trip-count floor stays sequential.
#[test]
fn below_threshold_for_range_skips_par_worker() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sq(int i) -> int {
    return i * i;
}
fn main() {
    let acc = 0;
    for x in 0..20 {
        acc = acc + sq(x);
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("short for-range should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_none(),
        "a 20-trip for-range cannot pay for a spawn"
    );
    assert_eq!(run_compiled(compiled, &pipeline, None), "2470");
}













#[test]
fn direct_class_field_access_avoids_temporary_object() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}
fn main() {
    let x = new Point(5, 6).x;
    write(stdout(), to_bytes(format("%i", x)));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "5");

    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("class field source");
    assert!(
        !compiled.insts("main").any(|i| allocates_object(i) || touches_field(i)),
        "direct class field access should not allocate or touch fields"
    );
}

/// Non-first field selection must index the staged ctor args (not always arg 0).
#[test]
fn direct_class_second_field_access_avoids_temporary_object() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}
fn main() {
    write(stdout(), to_bytes(format("%i", new Point(5, 6).y)));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "6");

    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("second field temp");
    assert!(
        !compiled.insts("main").any(|i| allocates_object(i) || touches_field(i)),
        "second-field temp elision must not allocate"
    );
}

/// `try_emit_direct_class_field_access` unwraps `Group`/`Expr` receivers.
#[test]
fn grouped_direct_class_field_access_avoids_temporary_object() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}
fn main() {
    write(stdout(), to_bytes(format("%i", (new Point(5, 6)).x)));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "5");

    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("grouped field temp");
    assert!(
        !compiled.insts("main").any(|i| allocates_object(i) || touches_field(i)),
        "grouped new Class(args).field must still elide"
    );
}

/// Q2: a named class used as a whole object materializes once.
#[test]
fn named_local_class_stays_heap_allocated() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}
fn take(Point p) -> int {
    return p.x;
}
fn main() {
    let p = new Point(5, 6);
    write(stdout(), to_bytes(format("%i", take(p))));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "5");

    let mut pipeline = test_pipeline();
    // `take` inlined, `p` is scalar-replaced: keep the call.
    pipeline.set_inline_max_cost(0);
    let compiled = compile(&mut pipeline, src).expect("named local class");
    assert_eq!(
        compiled.insts("main").filter(|i| allocates_object(i)).count(),
        1,
        "identity use must allocate once"
    );
}

/// An inherent method's `Option<int>` parameter takes its two words: the
/// caller passes the pair without boxing an `Option`.
#[test]
fn inherent_method_pair_parameter_takes_two_words() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class H {
    pub d: int,
}
impl H {
    pub fn get(Option<int> o) -> int {
        return match o {
            Some(v) => v,
            None => self.d,
        };
    }
}
fn main() {
    let h = new H(1);
    let s = 0;
    let i = 0;
    while i < 10 {
        let o = if i % 3 == 0 { None } else { Some(i) };
        s += h.get(o);
        i += 1;
    }
    let _ = write(stdout(), to_bytes(format("%i", s)));
}
"#;
    assert_eq!(run_example_src(src), "31");

    let mut pipeline = test_pipeline();
    // Keep the call: inlined, there is no edge to look at.
    pipeline.set_inline_max_cost(0);
    let compiled = compile(&mut pipeline, src).expect("inherent pair parameter");
    assert_eq!(compiled.body("H::get").params.len(), 3, "`get` takes `self` and `o` as two words");
    assert!(
        !compiled.insts("main").any(|i| matches!(i, MirInst::Alloc { kind: MirAllocKind::Enum { .. }, .. })),
        "`main` passes the pair without boxing it"
    );
}

/// A ground trait instance method returns its `Option<int>` as two words
/// to a direct call; its `$dict` adapter, which a dictionary holds, boxes
/// the pair.
#[test]
fn ground_instance_method_returns_two_words() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
trait Halver<T> {
    fn half(T s, int x) -> Option<int> {}
}
class H {
    pub d: int,
}
impl Halver for H {
    pub fn half(H h, int x) -> Option<int> {
        if x % 3 == 0 {
            return None;
        }
        return Some(x / h.d);
    }
}
fn via<T: Halver>(T h, int x) -> int {
    return match h.half(x) {
        Some(v) => v,
        None => 0,
    };
}
fn main() {
    let h = new H(2);
    let s = 0;
    let i = 0;
    while i < 10 {
        s += match h.half(i) {
            Some(v) => v,
            None => 1,
        };
        i += 1;
    }
    let _ = write(stdout(), to_bytes(format("%i %i", s, via(h, 8))));
}
"#;
    assert_eq!(run_example_src(src), "16 4");

    let mut pipeline = test_pipeline();
    // Keep the calls: inlined, there is no edge to look at.
    pipeline.set_inline_max_cost(0);
    let compiled = compile(&mut pipeline, src).expect("pair-returning instance method");
    let method = compiled.bodies.iter().find(|b| b.name.ends_with("::half") || b.name.ends_with("__half")).expect("the method body");
    assert!(method.mir.ret_hi_ty.is_some(), "`half` returns two words");
    assert!(
        !compiled.insts("main").any(|i| matches!(i, MirInst::Alloc { kind: MirAllocKind::Enum { .. }, .. })),
        "`main` reads the pair without a box"
    );
}

/// A free function's `(int, int)` / two-field readonly class parameter
/// takes its two words, as its result does: the caller passes them
/// without building a tuple or an instance.
#[test]
fn product_and_pair_class_parameters_take_two_words() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
readonly class Pt {
    pub x: int,
    pub y: int,
}
fn mk(int i) -> (int, int) {
    return (i, i + 1);
}
fn add((int, int) p) -> int {
    let (a, b) = p;
    return a + b;
}
fn norm1(Pt p) -> int {
    return p.x + p.y;
}
fn main() {
    let s = 0;
    let i = 0;
    while i < 10 {
        s += add(mk(i)) + add((i, 1)) + norm1(new Pt(i, 2));
        i += 1;
    }
    let _ = write(stdout(), to_bytes(format("%i", s)));
}
"#;
    assert_eq!(run_example_src(src), "220");

    let mut pipeline = test_pipeline();
    // Keep the calls: inlined, there is no edge to look at.
    pipeline.set_inline_max_cost(0);
    let compiled = compile(&mut pipeline, src).expect("pair parameters");
    assert_eq!(compiled.body("add").params.len(), 2, "`add` takes `p` as two words");
    assert_eq!(compiled.body("norm1").params.len(), 2, "`norm1` takes `p` as two words");
    assert!(
        !compiled
            .insts("main")
            .any(|i| matches!(i, MirInst::Alloc { kind: MirAllocKind::TUPLE | MirAllocKind::Object { .. }, .. })),
        "`main` passes the pairs without boxing them"
    );
}

#[test]
fn named_local_class_field_read_unboxes_when_no_escape() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}
fn main() {
    let p = new Point(5, 6);
    write(stdout(), to_bytes(format("%i", p.x)));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "5");

    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("unbox named local");
    assert!(
        !compiled.insts("main").any(|i| allocates_object(i) || touches_field(i)),
        "non-escaping named local field read should unbox"
    );
}

#[test]
fn named_local_class_field_store_unboxes_when_no_escape() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}
fn main() {
    let p = new Point(5, 6);
    p.x = 10;
    p.y = p.x + 1;
    write(stdout(), to_bytes(format("%i", p.x + p.y)));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "21");

    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("unbox field store");
    assert!(
        !compiled.insts("main").any(|i| allocates_object(i) || touches_field(i)),
        "non-escaping named local field store should unbox"
    );
}

/// `fn drop()` forces a real instance even for a consumed `new C(args).field`.
#[test]
fn drop_class_temp_field_access_still_allocates() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Handle { pub fd: int }
impl Handle {
    fn drop() {}
}
fn main() {
    write(stdout(), to_bytes(format("%i", new Handle(7).fd)));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "7");

    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("drop temp field");
    assert!(
        compiled.insts("main").any(allocates_object),
        "drop class temp must allocate in main so the finalizer can run"
    );
    assert!(
        compiled.insts("main").any(touches_field),
        "drop class temp must read the heap instance"
    );
}

#[test]
fn class_field_store_then_load_reads_the_store() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}
fn bump(Point p) -> int {
    p.x = 3;
    return p.x + p.y;
}
fn main() {
    write(stdout(), to_bytes(format("%i", bump(new Point(1, 2)))));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "5");
}

#[test]
fn record_field_access_reads_its_slot() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let d = { foo: 7 };
    write(stdout(), to_bytes(format("%i", d.foo)));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "7");
}

#[test]
fn array_argument_survives_collect_in_callee() {
    let src = r#"
use gc::{collect};
use io::{stdout, write};
use string::{format, to_bytes};
fn keep([int; 3] xs) -> [int; 3] {
    let junk = [1, 2, 3];
    collect();
    let r = xs;
    if xs.len() > 5 {
        r = junk;
    }
    return r;
}
fn main() {
    let a = [42, 7, 0];
    let b = keep(a);
    write(stdout(), to_bytes(format("%i", b[0])));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("compile");
    let out = run_compiled(compiled, &pipeline, None);
    assert_eq!(out, "42");
}

/// A function holding an object across a collection keeps it.
#[test]
fn object_held_across_collect_survives() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
use gc::{collect};
class B {
    pub v: int,
}
fn fib(int n) -> int {
    if n < 2 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}
fn holds(int n) -> int {
    let b = new B(n);
    collect();
    return b.v;
}
fn main() {
    write(stdout(), to_bytes(format("%i", fib(10) + holds(3))));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("compile");
    let out = run_compiled(compiled, &pipeline, None);
    assert_eq!(out, "58");
}

#[test]
fn archive_load_keeps_objects_across_collect() {
    let src = r#"
use gc::{collect};
use io::{stdout, write};
use string::{format, to_bytes};
fn keep([int; 3] xs) -> [int; 3] {
    let junk = [1, 2, 3];
    collect();
    if junk == xs {
        return junk;
    }
    return xs;
}
fn main() {
    let a = [42, 7, 0];
    let b = keep(a);
    write(stdout(), to_bytes(format("%i", b[0])));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("compile");
    let run = run_through_archive(compiled, &pipeline);
    assert!(!run.panicked, "archive-loaded keep/collect panicked");
    assert_eq!(run.output, "42");
}

#[test]
fn pair_of_ints_in_a_vec_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn pair(int a, int b) -> Vec<int> {
    return [a, b];
}
fn main() {
    let xs = pair(3, 4);
    write(stdout(), to_bytes(format("%i", xs[0] + xs[1])));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("compile");
    let out = run_compiled(compiled, &pipeline, None);
    assert_eq!(out, "7");
}

#[test]
fn nsieve_checksum() {
    let src = include_str!("../../examples/perf/nsieve.coil");
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("compile nsieve");
    let out = run_compiled(compiled, &pipeline, None);
    assert_eq!(out, "1900");
}

#[test]
fn call_result_in_an_array_literal_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn leaf(int n) -> int {
    return n + 1;
}
fn wrap(int n) -> Vec<int> {
    return [leaf(n), n];
}
fn main() {
    let xs = wrap(2);
    write(stdout(), to_bytes(format("%i", xs[0] + xs[1])));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("compile");
    let out = run_compiled(compiled, &pipeline, None);
    assert_eq!(out, "5");
}








#[test]
fn thread_main_exits_without_join_still_runs_worker_recv() {
    // Regression: process exit used to kill workers still in `recv`, so
    // nothing after the worker's recv ran and the script looked like it
    // "didn't block". Auto-join at end of `run_with_pool` keeps them alive.
    let output = run_example_src(
        r#"
use thread::{channel, recv, send, spawn, Receiver};
use io::{stdout, write};
use string::{format, to_bytes};

fn worker(Receiver rx) {
    let msg = recv(rx)?;
    write(stdout(), to_bytes(format("%s", msg)));
    return 0;
}

fn main() {
    let pair = channel()?;
    // spawn fails with ThreadError, channel/send with ChannelError.
    let _ = spawn(worker, pair[1]);
    send(pair[0], "hi")?;
}
"#,
    );
    assert_eq!(output, "hi");
}

#[test]
fn nested_spawn_joins_via_shared_root_registry() {
    // Worker mid spawns leaf on the root runtime's live-thread registry.
    // Main returns without join; root auto-join must still wait for leaf.
    let output = run_example_src(
        r#"
use thread::{spawn};
use io::{stdout, write};
use string::{format, to_bytes};

fn leaf() {
    write(stdout(), to_bytes("leaf"));
    return 0;
}

fn mid() {
    let _ = spawn(leaf)?;
    return 0;
}

fn main() {
    let _ = spawn(mid)?;
}
"#,
    );
    assert_eq!(output, "leaf");
}
















#[test]
fn aggregate_float_negate_negates_as_float() {
    // Regression: float aggregate unary `-` must not negate as an int
    // (which bit-twiddles a float as i64). The elements are read from a
    // static something writes: literals (and a static nothing writes) fold
    // at compile time.
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
static let half: float = 1.5;
fn reset() { half = 1.5; }
fn main() {
    let d = -(half, half + 0.5);
    write(stdout(), to_bytes(format("%f,%f", d[0], d[1])));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("compile");
    assert_eq!(
        run_compiled(compiled, &pipeline, None),
        "-1.5,-2.0"
    );
}








/// HostInvoke + virtual `env::args`: `?` yields argv; argv0 matches process.
#[test]
fn env_args_ok_has_argv0_via_host_invoke() {
    let output = run_example_src(
        r#"
use env::{args};
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let a = args()?;
    write(stdout(), to_bytes(format("%s", a[0])));
}
"#,
    );
    let expect = std::env::args().next().expect("process argv0");
    assert_eq!(output, expect, "args()? argv0 must match process");
}

/// HostInvoke + `match args()`: Ok arm binds a usable string vector.
#[test]
fn env_args_match_ok_exposes_argv_len() {
    let output = run_example_src(
        r#"
use env::{args};
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let n = match args() {
        Result::Ok(v) => v.len(),
        Result::Err(_) => -1,
    };
    write(stdout(), to_bytes(format("%i", n)));
}
"#,
    );
    let n: i64 = output.parse().expect("argv length");
    let expect = std::env::args().count() as i64;
    assert_eq!(n, expect, "match Ok len must match process argc");
}

/// Generic Stream.attach / Stream.park are compiler-known `io` methods.
#[test]
fn stream_attach_and_park_typecheck() {
    fn check_ok(src: &str) {
        let mut pipeline = test_pipeline();
        assert!(
            pipeline.compile_src(src).is_ok(),
            "expected typecheck Ok for {src:?}, messages={:?}",
            pipeline.messages()
        );
    }
    fn check_ok_attach(src: &str) {
        let mut pipeline = test_pipeline();
        pipeline.grant_attach();
        assert!(
            pipeline.compile_src(src).is_ok(),
            "expected typecheck Ok for {src:?}, messages={:?}",
            pipeline.messages()
        );
    }
    check_ok("use io::{attach, park};\nfn main() {}\n");
    check_ok(
        r#"
use io::{stdout, park};
fn main() {
    let s = stdout();
    let _ = s.park();
}
"#,
    );
    check_ok_attach(
        r#"
use io::{stdout, attach, park};
fn main() {
    let s = stdout();
    let _ = attach(s, 1, 2, 3, 4, 5);
    let _ = s.park();
}
"#,
    );
    check_ok_attach(
        r#"
use io::{stdout};
fn main() {
    let s = stdout();
    let _ = s.attach(1, 2, 3, 4, 5);
    let _ = s.park();
}
"#,
    );
}

#[test]
fn stream_attach_invalid_when_allow_attach() {
    let src = r#"
use io::{stdout, write, attach, IoError};
use string::{format, to_bytes};
fn main() {
    let s = stdout();
    let r = attach(s, 0, 0, 0, 0, 0);
    let msg = match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => match e {
            IoError::PermissionDenied => "denied",
            IoError::InvalidInput => "invalid",
            default => "other",
        },
    };
    write(stdout(), to_bytes(format("%s", msg)));
}
"#;
    let mut grants = compiler::HostGrants::deny_all();
    grants.allow_attach = true;
    let output = run_userland_dload_project_grants(
        "allow_attach_null",
        src,
        DloadFlags::default(),
        grants,
    );
    assert_eq!(
        output, "invalid",
        "attach must reach pointer checks, got {output:?}"
    );
}

/// Root / compiler / machine / CLI default features are empty (no virtual time).
#[test]
fn language_default_features_are_empty() {
    let root = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../Cargo.toml"));
    let compiler = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    let machine = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../machine/Cargo.toml"
    ));
    let cli = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../coil-cli/Cargo.toml"
    ));
    for (label, toml) in [
        ("root", root),
        ("compiler", compiler),
        ("machine", machine),
        ("cli", cli),
    ] {
        assert!(
            toml.contains("default = []"),
            "{label} default features must be []"
        );
        assert!(
            !toml.lines().any(|l| l.trim_start().starts_with("time = ")),
            "{label} must not declare a time cargo feature"
        );
        assert!(
            !toml.contains("chrono"),
            "{label} must not depend on chrono"
        );
        assert!(
            !toml.contains("default = [\"crypto\""),
            "{label} default features must not include crypto"
        );
    }
    let ci = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../.github/workflows/ci.yml"
    ));
    assert!(
        !ci.contains("--features time") && !ci.contains("features time"),
        "CI must not compile-gate a time cargo feature"
    );
}






/// #1558: `static const` still compiles for one release, with an E0129
/// warning that says to write `const`; a `const` that does not fold has no
/// warning.
#[test]
fn static_const_is_deprecated() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
fn two() -> int {
    return 2;
}

static const OLD = 7;
const NEW = two();

fn main() {
    let n = OLD + NEW;
}
"#,
    );
    assert!(result.is_ok(), "static const is a warning, not an error");
    let deprecated: Vec<_> = pipeline
        .messages()
        .iter()
        .filter(|m| m.code() == Some(compiler::ErrorCode::Deprecated))
        .collect();
    assert_eq!(deprecated.len(), 1, "{deprecated:?}");
    assert_eq!(*deprecated[0].kind(), reporting::MessageKind::WARNING);
    assert!(deprecated[0].message().contains("static const"), "{deprecated:?}");
}

#[test]
fn unreachable_after_return_is_warning() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn f() -> int {
    return 1;
    write(stdout(), to_bytes("dead"));
}

fn main() {
    write(stdout(), to_bytes(format("%i", f())));
}
"#,
    );
    assert!(result.is_ok(), "unreachable is warning, not hard error");
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::UnreachableCode)
                && *m.kind() == reporting::MessageKind::WARNING)
    );
}

#[test]
fn unreachable_after_while_true_is_warning() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn f() -> int {
    while true {
    }
    write(stdout(), to_bytes("dead"));
}

fn main() {
    let _ = f;
}
"#,
    );
    assert!(result.is_ok(), "unreachable after infinite loop is warning");
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::UnreachableCode)
                && *m.kind() == reporting::MessageKind::WARNING)
    );
}

#[test]
fn defer_dominated_by_while_true_warns_e0123() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
use io::{stdout, write};
use string::{to_bytes};
fn f() -> int {
    defer { write(stdout(), to_bytes("d")); }
    while true {
    }
}

fn main() {
    let _ = f;
}
"#,
    );
    // Warnings stay inspectable; only hard errors fail `compile_src`.
    assert!(
        result.is_ok(),
        "warning-only E0123 must not fail compile_src: {:?}",
        pipeline.messages()
    );
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::DeferNeverRuns)
                && *m.kind() == reporting::MessageKind::WARNING),
        "expected DeferNeverRuns dominated by while true"
    );
    assert!(
        !pipeline
            .messages()
            .iter()
            .any(|m| *m.kind() == reporting::MessageKind::ERROR),
        "E0123 should be warning-only: {:?}",
        pipeline.messages()
    );
    assert!(!pipeline.had_errors());
}

#[test]
fn defer_inside_while_true_body_does_not_warn_e0123() {
    // The loop body's block ends every iteration, which runs its defer.
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
use io::{stdout, write};
use string::{to_bytes};
fn f() -> int {
    while true {
        defer { write(stdout(), to_bytes("d")); }
    }
}

fn main() {
    let _ = f;
}
"#,
    );
    assert!(result.is_ok(), "{:?}", pipeline.messages());
    assert!(
        !pipeline
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::DeferNeverRuns)),
        "a loop body's defer runs each iteration: {:?}",
        pipeline.messages()
    );
}





/// `if (!c) { A } else { B }` inverts to `if (c) { B } else { A }`: the arms
/// must not swap incorrectly.
#[test]
fn invert_not_if_else_preserves_arm_semantics() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn pick(bool c) -> int {
    if !c {
        return 10;
    } else {
        return 20;
    }
}
fn main() {
    write(stdout(), to_bytes(format("%i,", pick(false))));
    write(stdout(), to_bytes(format("%i", pick(true))));
}
"#;
    assert_eq!(run_example_src(src), "10,20");
}

/// Field and index assignments as statements.
#[test]
fn field_and_index_assignment_statements_run() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}
fn main() {
    let p = new Point(0, 0);
    p.x = 3;
    p.y = 4;
    let a = [0, 0];
    a[0] = 10;
    a[1] = 20;
    write(stdout(), to_bytes(format("%i,", p.x + p.y)));
    write(stdout(), to_bytes(format("%i", a[0] + a[1])));
}
"#;
    assert_eq!(run_example_src(src), "7,30");
}


/// for-in over arrays hoists the length out of the loop header.
#[test]
fn for_in_array_hoists_array_len_once() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let a = [1, 2, 3];
    for x in a {
        write(stdout(), to_bytes(format("%i", x)));
    }
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("compile");
    let lens = compiled
        .insts("main")
        .filter(|i| matches!(i, MirInst::ArrayLen { .. }))
        .count();
    assert_eq!(lens, 1, "main for-in should read the length once before the loop");
    assert_eq!(run_example_src(src), "123");
}


/// String table: serialize → load → run must keep string indexes valid.
#[test]
fn string_table_archive_round_trip_preserves_literals() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    write(stdout(), to_bytes(format("%s-%i", "archive", 35)));
}
"#;
    let mut pipeline = test_pipeline();
    let compiled = compile(&mut pipeline, src).expect("compile");
    assert!(
        pipeline.strings().iter().any(|s| s == "archive"),
        "string table should contain literal `archive`: {:?}",
        pipeline.strings()
    );
    assert_eq!(run_through_archive(compiled, &pipeline).output, "archive-35");
}

/// Two concatenations in separate statements (one chain `a + b + c` is a
/// single format).
#[test]
fn nested_string_concat_runs() {
    let src = r#"
use io::{stdout, write};
use string::to_bytes;

fn main() {
    let a = "a";
    let b = "b";
    let c = "c";
    let ab = a + b;
    write(stdout(), to_bytes(ab + c));
}
"#;
    assert_eq!(run_example_src(src), "abc");
}


/// COI-18: in-memory compile must not fail closed on warnings alone.
#[test]
fn compile_src_from_file_succeeds_with_only_warnings() {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("coil_coi18_{pid}_{nanos}"));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let src_path = dir.join("warn_only.coil");
    std::fs::write(
        &src_path,
        r#"
fn main() {
    return;
    let _ = 1;
}
"#,
    )
    .expect("write warn_only.coil");

    let mut pipeline = Pipeline::new();
    pipeline.bind_project_roots_with_default(dir.clone(), Vec::<std::path::PathBuf>::new());
    let result = pipeline.compile_src_from_file(src_path.to_str().unwrap());
    let msgs: Vec<_> = pipeline.messages().iter().map(|m| m.message()).collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        result.is_ok(),
        "warnings (unreachable code) must not fail in-memory compile: {msgs:?}"
    );
    assert!(
        !pipeline.had_errors(),
        "pipeline should report no hard errors"
    );
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| *m.kind() == reporting::MessageKind::WARNING
                && m.message().contains("unreachable")),
        "expected unreachable-code warning to remain inspectable: {msgs:?}"
    );
}



/// COI-19: an `extern` block in an imported module, with a `Vec` allocation
/// between two calls.
#[cfg(unix)]
#[test]
fn example_ffi_mod_entry_prints_10() {
    let libsum = ensure_ffi_libsum_built();
    if !libsum.exists() {
        ffi_soft_skip(&format!("{} not built", libsum.display()));
        return;
    }
    let output = run_ffi_example_with_lib("examples/ffi_mod_entry.coil", &libsum);
    assert_eq!(output, "10");
}

/// Codegen must not depend on hash-set iteration order (finalizer registry).
#[test]
fn compile_is_deterministic_for_many_drop_classes() {
    let mut src = String::new();
    for i in 0..12 {
        src.push_str(&format!(
            "class C{i} {{ pub v: int, }}\nimpl C{i} {{ fn drop() {{ }} }}\n"
        ));
    }
    src.push_str("fn main() {\n");
    for i in 0..12 {
        src.push_str(&format!("    let _c{i} = new C{i}({i});\n"));
    }
    src.push_str("}\n");
    let compile = || {
        let mut pipeline = test_pipeline();
        let compiled = compile(&mut pipeline, &src).expect("drop classes compile");
        compiler::native_mir::archive::encode(&compiled.bodies, &body_ids(&pipeline), &[])
    };
    let first = compile();
    for _ in 0..4 {
        assert!(compile() == first, "MIR differs between identical compiles");
    }
}

/// The archive of a program built from several modules is the same bytes on
/// every compile. String, label, static and clone ids are handed out as
/// lowering reaches each use, so nothing on that path may follow hash order.
#[test]
fn compile_is_deterministic_for_multi_module_programs() {
    use compiler::native_mir::archive;
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("workspace root");
    // Programs over the stdlib and `examples/src`: generics and their
    // clones, trait dictionaries, derives, macros, closures, generators,
    // contracts, hash maps, string formatting.
    let programs = [
        "examples/perf/text_scan_long.coil",
        "examples/perf/generic_many_types.coil",
        "examples/derive_show_eq.coil",
        "examples/modules_brace.coil",
        "examples/macro_fn.coil",
        "tests/macros/attr_macro.coil",
        "tests/comptime/generated_macros.coil",
        "tests/programs/bits_and_floats.coil",
        "tests/programs/priority_queue.coil",
        "tests/programs/gen_fold_zip.coil",
        "tests/programs/pipeline_ranges.coil",
        "tests/collections/hashmap_for_in_entry_types.coil",
        "tests/traits/trait_method_bound_dispatch.coil",
        "tests/contracts/contract_failure_messages.coil",
        "tests/representation/drop_enum__hashmap_entry.coil",
    ];
    for program in programs {
        let path = workspace_root.join(program);
        let archive = || {
            let mut pipeline = test_pipeline();
            let compiled = compile_file(&mut pipeline, path.to_str().expect("utf-8 path")).unwrap_or_else(|_| panic!("{program} failed to compile"));
            let program = pipeline.archive(compiled.constants, |_| archive::encode(&compiled.bodies, &body_ids(&pipeline), &[]));
            rkyv::to_bytes::<rkyv::rancor::Error>(&program).expect("serialize").to_vec()
        };
        let first = archive();
        for _ in 0..2 {
            assert!(archive() == first, "{program}: the archive differs between identical compiles");
        }
    }
}

/// E3: a function whose effects exceed its `pure fn` / `uses {…}` (or its
/// trait method's) does not compile.
#[test]
fn broken_effect_declarations_are_errors() {
    let broken = [
        (
            "static let HITS: int = 0;\nfn bump() {\n    HITS = HITS + 1;\n}\npure fn twice(int x) -> int {\n    bump();\n    return x * 2;\n}\n",
            "`twice` is declared `pure` but needs read, mutate: twice → bump needs",
        ),
        (
            "use io::stdout;\nuse io::sync::write_all;\nuse string::to_bytes;\nfn say(string s) uses {suspend} {\n    write_all(stdout(), to_bytes(s));\n}\n",
            "`say` is declared `uses {suspend}` but needs write",
        ),
        (
            "static let HITS: int = 0;\ntrait Area<A> {\n    pure fn area(A self) -> int;\n}\nclass Sq {\n    pub side: int,\n}\nimpl Area for Sq {\n    fn area(Sq self) -> int {\n        HITS = HITS + 1;\n        return self.side;\n    }\n}\n",
            "`Area for Sq::area` implements `Area::area`, declared `pure` but needs read, mutate",
        ),
    ];
    {
        for (src, expected) in broken {
            let mut pipeline = test_pipeline();
            assert_compile_fails_pipeline(&mut pipeline, src, compiler::ErrorCode::EffectMismatch);
            assert!(
                pipeline.messages().iter().any(|m| m.message().contains(expected)),
                "expected `{expected}` in {:?}",
                pipeline.messages().iter().map(|m| m.message().to_string()).collect::<Vec<_>>()
            );
        }
        let kept = "trait Area<A> {\n    pure fn area(A self) -> int;\n}\nclass Sq {\n    pub side: int,\n}\nimpl Area for Sq {\n    fn area(Sq self) -> int {\n        return self.side * self.side;\n    }\n}\npure fn total(Area a, Area b) -> int {\n    if !(true) {\n        panic \"assertion failed\";\n    }\n    return a.area() + b.area();\n}\nfn apply(int -> int f, int x) -> int uses {} {\n    return f(x);\n}\n";
        let mut pipeline = test_pipeline();
        compile_ok(&mut pipeline, kept);
    }
}

/// E4: each gated host call the program reaches needs its capability, with
/// the call chain in the message; one nothing reaches
/// needs none.
#[test]
fn reachable_host_calls_need_their_capabilities() {
    let src = "use env::{set_var, exit};\nuse io::{open};\nuse io::fs::{exists, remove_file};\nfn shell() {\n    let _ = set_var(\"A\", \"b\");\n}\nfn load() -> bool {\n    let _ = open(\"cfg.toml\", \"r\");\n    let _ = exists(\"cfg.toml\");\n    return true;\n}\nfn main() {\n    if load() {\n        exit(0);\n    }\n}\n";
    let unused_cleanup = "fn cleanup() {\n    let _ = remove_file(\"x\");\n}\n";
    let src = format!("{src}{unused_cleanup}");
    {
        let mut pipeline = deny_pipeline();
        assert!(pipeline.compile_src(&src).is_err());
        let msgs: Vec<(Option<compiler::ErrorCode>, String)> = pipeline
            .messages()
            .iter()
            .map(|m| (m.code(), m.message().to_string()))
            .collect();
        let has = |code, text: &str| msgs.iter().any(|(c, m)| *c == Some(code) && m.contains(text));
        assert!(has(compiler::ErrorCode::HostExitDenied, "`env::exit` requires `--allow-exit`: reached from `main`"), "{msgs:?}");
        assert!(has(compiler::ErrorCode::HostCapDenied, "`io::open` requires `--allow-read`: reached from `main` → `load`"), "{msgs:?}");
        assert!(has(compiler::ErrorCode::HostCapDenied, "`io::fs::exists` requires `--allow-read`"), "{msgs:?}");
        assert!(!msgs.iter().any(|(_, m)| m.contains("env::set_var") || m.contains("remove_file")), "unreached calls are free: {msgs:?}");

        let mut pipeline = deny_pipeline();
        assert!(pipeline.grant_capability("read"));
        pipeline.grant_exit();
        compile_ok(&mut pipeline, &src);
    }
}

/// E4: a gated call in a callee that typed inlining spliced into `main` is
/// still reached, through the callee it was written in (the callee's own,
/// now unreached, body must not hide it).
#[test]
fn inlined_host_calls_still_need_their_capabilities() {
    let src = "use io::fs::{exists};\nfn probe() -> int {\n    let _ = exists(\"cfg.toml\");\n    return 1;\n}\nfn main() {\n    if probe() == 1 {\n        return 1;\n    }\n    return 0;\n}\n";
    let mut pipeline = deny_pipeline();
    assert!(pipeline.compile_src(src).is_err());
    let msgs: Vec<String> = pipeline.messages().iter().map(|m| m.message().to_string()).collect();
    assert!(
        msgs.iter().any(|m| m == "`io::fs::exists` requires `--allow-read`: reached from `main` → `probe`"),
        "{msgs:?}"
    );
}

/// E4: `open` needs read or write by its literal mode, both when the mode
/// is not a literal; a static initializer always runs.
#[test]
fn open_mode_and_static_initializers_decide_capabilities() {
    // `mode` hides its literal behind a `Vec`, which inlining keeps.
    let src = "use io::{open};\nstatic let LOG = open(\"log.txt\", \"a\");\nfn mode() -> string {\n    let v: Vec<string> = Vec::new();\n    v.push(\"r\");\n    return v[0];\n}\nfn main() {\n    let _ = open(\"in.txt\", mode());\n}\n";
    let mut pipeline = deny_pipeline();
    assert!(pipeline.compile_src(src).is_err());
    let msgs: Vec<String> = pipeline.messages().iter().map(|m| m.message().to_string()).collect();
    assert!(msgs.iter().any(|m| m == "`io::open` requires `--allow-write`: reached from the initializer of static `LOG`"), "{msgs:?}");
    assert!(msgs.iter().any(|m| m.starts_with("`io::open` requires `--allow-read --allow-write`: reached from `main`")), "{msgs:?}");
    let mut pipeline = deny_pipeline();
    pipeline.grant_capability("write");
    assert!(pipeline.compile_src(src).is_err(), "the non-literal mode still needs read");
    let mut pipeline = deny_pipeline();
    pipeline.grant_all();
    compile_ok(&mut pipeline, src);
}

/// E4: under `coil test` the tests are the entry points, named by their
/// description in the chain.
#[test]
fn tests_are_entry_points_for_capabilities() {
    let src = "use io::fs::{exists};\nfn probe() -> bool {\n    let _ = exists(\"x\");\n    return true;\n}\ntest(\"looks for x\") {\n    if !(probe()) { raise \"assertion failed\"; }\n}\n";
    let mut pipeline = deny_pipeline();
    pipeline.set_include_tests(true);
    assert!(pipeline.compile_src(src).is_err());
    let msgs: Vec<String> = pipeline.messages().iter().map(|m| m.message().to_string()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("reached from test \"looks for x\" → `probe`")),
        "{msgs:?}"
    );
}

/// E4: a gated call a macro wrote is checked as one the user wrote. Its
/// span is the expansion's, not a place in a file, and the check finds the
/// call's `HostInvoke` by that span: in an expression where the macro is
/// used, and in a function the macro declares.
#[test]
fn macro_generated_host_calls_still_need_their_capabilities() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("coil_gated_macros_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    let provider = "use macro::{Expr, Code};\n\nmacro probe(Expr path) -> Code {\n    return quote expr { exists(${path}) };\n}\n\nmacro prober(Expr name) -> Code {\n    return quote items {\n        fn ${name}() -> bool {\n            let _ = exists(\"gen.toml\");\n            return true;\n        }\n    };\n}\n";
    let main = "use io::fs::{exists};\nuse gated_macros::{probe, prober};\n\nprober!(generated);\n\nfn main() {\n    let _ = probe!(\"cfg.toml\");\n    if generated() {\n        return;\n    }\n}\n";
    std::fs::write(dir.join("src/gated_macros.coil"), provider).expect("write the provider");
    let file = dir.join("main.coil");
    std::fs::write(&file, main).expect("write the program");
    let pipeline = |grant: bool| {
        let mut pipeline = deny_pipeline();
        pipeline.bind_project_roots_with_default(dir.clone(), Pipeline::workspace_language_extra_roots());
        if grant {
            assert!(pipeline.grant_capability("read"));
        }
        pipeline
    };
    let mut denied = pipeline(false);
    assert!(denied.compile_src_from_file(file.to_str().expect("utf-8 path")).is_err(), "compiled with no grant");
    let msgs: Vec<String> = denied.messages().iter().map(|m| m.message().to_string()).collect();
    assert!(msgs.iter().any(|m| m == "`io::fs::exists` requires `--allow-read`: reached from `main`"), "{msgs:?}");
    assert!(msgs.iter().any(|m| m == "`io::fs::exists` requires `--allow-read`: reached from `main` → `generated`"), "{msgs:?}");
    let mut granted = pipeline(true);
    assert!(granted.compile_src_from_file(file.to_str().expect("utf-8 path")).is_ok(), "{:?}", granted.messages().iter().map(|m| m.message().to_string()).collect::<Vec<_>>());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Compile and run with contract checks at `level`, from a file so panics
/// name their locations (`contracts.coil:LINE:COL`).
fn run_contracts_src(src: &str, level: compiler::ContractLevel) -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "coil_contracts_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("contracts.coil");
    std::fs::write(&file, src).expect("write source");
    let mut pipeline = test_pipeline();
    pipeline.set_contracts(level);
    let compiled = compile_file(&mut pipeline, file.to_str().expect("utf-8 path"))
        .expect("compile");
    let (run, _) = run_as_case(compiled, &pipeline, Some(file.as_path()));
    let _ = std::fs::remove_dir_all(&dir);
    run.output
}

const CONTRACTS_SRC: &str = r#"
use io::{stdout, write};
use string::{format, to_bytes};

fn say(string s) {
    write(stdout(), to_bytes(s));
}

fn num(int n) -> string {
    return format("%i", n);
}

fn half(int n) -> int
    requires n >= 0, "negative"
{
    return n / 2;
}

fn clamp(int n) -> int
    ensures result >= 0 && result <= 10
{
    if n < 0 {
        return n;
    }
    return match n {
        0 => 0,
        default => cap(n),
    };
}

fn cap(int n) -> int {
    if n > 10 {
        return 10;
    }
    return n;
}

fn parse(int n) -> Result<int, string>
    ensures match result { Result::Ok(v) => v > 0, Result::Err(_) => true }
{
    let v = checked(n)?;
    return Result::Ok(v - 5);
}

fn checked(int n) -> Result<int, string> {
    if n < 0 {
        return Result::Err("neg");
    }
    return Result::Ok(n);
}

class Counter {
    pub n: int,
}

impl Counter {
    pub fn bump(int by)
        requires by > 0
        ensures self.n > 0
    {
        self.n = self.n + by;
    }
}

fn main() {
    say(num(half(8)));
    say(num(clamp(4)) + num(clamp(40)));
    let c = new Counter(0);
    c.bump(2);
    say(num(c.n));
    let ok = match parse(-1) {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
    say(ok);
    say("|");
    CASE
}
"#;

fn contracts_case(case: &str, level: compiler::ContractLevel) -> String {
    run_contracts_src(&CONTRACTS_SRC.replace("CASE", case), level)
}

#[test]
fn a_failed_requires_blames_the_caller() {
    let out = contracts_case("say(num(half(-2)));", compiler::ContractLevel::All);
    assert!(
        out.starts_with("44102neg|panic: contract violated: requires n >= 0 (\"negative\") in half, called from "),
        "got {out:?}"
    );
    // The call site's line (main's last statement), not the clause's.
    assert!(out.contains("contracts.coil:77:"), "got {out:?}");
    let out = contracts_case("c.bump(0);", compiler::ContractLevel::All);
    assert!(out.contains("requires by > 0 in Counter::bump, called from "), "got {out:?}");
}

#[test]
fn contract_levels_skip_clauses() {
    let out = contracts_case("say(num(clamp(-3)));", compiler::ContractLevel::Requires);
    assert_eq!(out, "44102neg|-3");
    let out = contracts_case("say(num(half(-2)));", compiler::ContractLevel::Off);
    assert_eq!(out, "44102neg|-1");
}

#[test]
fn loop_invariants_and_decreases_are_checked() {
    let src = r#"
fn sum_to(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n
        invariant i <= n
        decreases n - i
    {
        s = s + i;
        i = i + 1;
    }
    return s;
}

fn stuck(int n) {
    let i = 0;
    while i < n
        decreases n - i
    {
        if i == 2 {
            continue;
        }
        i = i + 1;
    }
}

fn total(Vec<int> xs) -> int {
    let t = 0;
    for x in xs
        invariant t >= 0, "only positives"
    {
        t = t + x;
    }
    return t;
}

fn main() {
    if !(sum_to(5) == 10) {
        panic "assertion failed";
    }
    let v: Vec<int> = Vec::new();
    v.push(3);
    v.push(4);
    if !(total(v) == 7) {
        panic "assertion failed";
    }
    let w: Vec<int> = Vec::new();
    w.push(3);
    w.push(-9);
    CASE
}
"#;
    use compiler::ContractLevel::{All, Requires};
    assert_eq!(run_contracts_src(&src.replace("CASE", ""), All), "");
    // The invariant fails after the last item, outside the loop.
    let out = run_contracts_src(&src.replace("CASE", "total(w);"), All);
    assert!(
        out.contains("contract violated: invariant t >= 0 (\"only positives\") in total"),
        "got {out:?}"
    );
    let out = run_contracts_src(&src.replace("CASE", "stuck(5);"), All);
    assert!(out.contains("contract violated: decreases n - i (did not decrease) in stuck"), "got {out:?}");
    // Loop clauses are checked with `all` only.
    assert_eq!(run_contracts_src(&src.replace("CASE", "total(w);"), Requires), "");
}

#[test]
fn class_invariants_hold_after_construction_and_pub_methods() {
    let src = r#"
class Account
    invariant self.balance >= 0, "no overdraft"
{
    balance: int,
}

impl Account {
    pub static fn open(int start) -> Account {
        return new Account(start);
    }

    pub fn deposit(int amount) {
        self.balance = self.balance + amount;
    }

    pub fn withdraw(int amount) -> int {
        // A private helper may break the invariant for a while.
        self.take(amount + 1);
        self.balance = self.balance + 1;
        return self.balance;
    }

    fn take(int amount) {
        self.balance = self.balance - amount;
    }
}

fn main() {
    let a = Account::open(10);
    a.deposit(5);
    if !(a.withdraw(15) == 0) {
        panic "assertion failed";
    }
    CASE
}
"#;
    use compiler::ContractLevel::{All, Requires};
    assert_eq!(run_contracts_src(&src.replace("CASE", ""), All), "");
    let out = run_contracts_src(&src.replace("CASE", "a.withdraw(1);"), All);
    assert!(
        out.contains("contract violated: invariant self.balance >= 0 (\"no overdraft\") in Account::withdraw"),
        "got {out:?}"
    );
    let out = run_contracts_src(&src.replace("CASE", "let _ = Account::open(-1);"), All);
    assert!(out.contains("in new Account"), "got {out:?}");
    assert_eq!(run_contracts_src(&src.replace("CASE", "a.withdraw(1);"), Requires), "");
    assert_compile_fails(
        "class C\n    invariant self.n\n{\n    pub n: int,\n}\n\nfn main() {\n    let _ = new C(1);\n}\n",
        compiler::ErrorCode::TypeMismatch,
    );
}

#[test]
fn trait_method_contracts_apply_to_every_impl() {
    let src = r#"
trait Area<T> {
    fn area(T x) -> int
        requires x > 0, "positive"
        ensures result >= 0
    {}
}

impl Area for int {
    pub fn area(int n) -> int ensures result < 100 {
        return n * n - 5;
    }
}

fn show<T: Area>(T x) -> int {
    return x.area();
}

fn main() {
    if !(show(3) == 4) {
        panic "assertion failed";
    }
    CASE
}
"#;
    use compiler::ContractLevel::{All, Requires};
    assert_eq!(run_contracts_src(&src.replace("CASE", ""), All), "");
    // The trait's `requires` blames the caller, under the impl's parameter name.
    let out = run_contracts_src(&src.replace("CASE", "show(0);"), All);
    assert!(
        out.contains("contract violated: requires x > 0 (\"positive\") in Area for int::area, called from "),
        "got {out:?}"
    );
    // The trait's `ensures`, reported at the impl method.
    let out = run_contracts_src(&src.replace("CASE", "show(2);"), All);
    assert!(
        out.contains("contract violated: ensures result >= 0 in Area for int::area at ") && out.contains("contracts.coil:10:4"),
        "got {out:?}"
    );
    // The impl's own `ensures` is checked too.
    let out = run_contracts_src(&src.replace("CASE", "show(11);"), All);
    assert!(out.contains("contract violated: ensures result < 100 in Area for int::area"), "got {out:?}");
    assert_eq!(run_contracts_src(&src.replace("CASE", "show(2);"), Requires), "");
    // An impl cannot strengthen the precondition.
    assert_compile_fails(
        "trait Area<T> {\n    fn area(T x) -> int requires x > 0 {}\n}\n\nimpl Area for int {\n    pub fn area(int n) -> int requires n < 10 { return n; }\n}\n\nfn main() {\n    let _ = area(2);\n}\n",
        compiler::ErrorCode::GenericTypeError,
    );
}

/// #890: a call to an import from a missing module reports the module
/// error only, not also the internal "cannot lower `main` yet" (E0801).
#[test]
fn missing_module_reports_no_internal_lowering_error() {
    let dir = std::env::temp_dir().join(format!("coil-890-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let entry = dir.join("main.coil");
    std::fs::write(&entry, "use nothere::x;\nfn main() {\n    x();\n}\n").expect("write entry");
    let mut pipeline = test_pipeline();
    assert!(pipeline.compile_src_from_file(entry.to_str().expect("utf-8")).is_err());
    let errors: Vec<_> = pipeline
        .messages()
        .iter()
        .filter(|m| *m.kind() == reporting::MessageKind::ERROR)
        .map(|m| (m.code(), m.message().to_string()))
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(errors.iter().any(|(_, m)| m.contains("Module not found")), "{errors:?}");
    assert!(!errors.iter().any(|(_, m)| m.contains("cannot lower")), "{errors:?}");
}

/// #2536: modules that import each other are one E0154 naming the cycle,
/// at the import that starts it, not an E0100 for a name the other module
/// does declare.
#[test]
fn import_cycle_is_one_error_naming_its_path() {
    let dir = std::env::temp_dir().join(format!("coil-2536-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let ca = "use cb::twice;\n\nclass Box1 {\n    pub n: int,\n}\n\nfn box_twice(Box1 b) -> int {\n    return twice(b);\n}\n";
    std::fs::write(dir.join("ca.coil"), ca).expect("write ca");
    std::fs::write(dir.join("cb.coil"), "use ca::Box1;\n\nfn twice(Box1 b) -> int {\n    return b.n * 2;\n}\n").expect("write cb");
    let entry = dir.join("main.coil");
    std::fs::write(&entry, "use ca::{Box1, box_twice};\n\nfn main() {\n    let _ = box_twice(new Box1(3));\n}\n").expect("write entry");
    let mut pipeline = test_pipeline();
    pipeline.bind_project_root(dir.clone(), vec![dir.clone()]);
    assert!(pipeline.compile_src_from_file(entry.to_str().expect("utf-8")).is_err());
    let errors: Vec<_> = pipeline
        .messages()
        .iter()
        .filter(|m| *m.kind() == reporting::MessageKind::ERROR)
        .map(|m| (m.code(), m.message().to_string(), m.range().clone()))
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(errors.len(), 1, "{errors:?}");
    let (code, message, range) = &errors[0];
    assert_eq!(*code, Some(compiler::ErrorCode::ImportCycle), "{errors:?}");
    assert!(message.contains("`ca -> cb -> ca`"), "{errors:?}");
    assert_eq!(&ca[range.clone()], "use cb::twice;", "{errors:?}");
}

#[test]
fn gc_finalizer_runs_on_teardown() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{to_bytes};
static let drops: int = 0;
class Handle { pub fd: int }
impl Handle {
    fn drop() {
        write(stdout(), to_bytes("closed"));
        drops = drops + 1;
    }
}
fn make() {
    let h = new Handle(1);
}
fn main() {
    make();
}
"#,
    );
    assert_eq!(output, "closed");
}

/// A lambda whose result moves as two words (`Option<int>`) gets a twin
/// returning the two words, and a call through the function value reads
/// them with a two-word `CallIndirect`: neither side boxes the result.
#[test]
fn function_value_pair_result_takes_two_words() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn run(int -> Option<int> f) -> int {
    let s = 0;
    let i = 0;
    while i < 10 {
        s += match f(i) {
            Some(v) => v,
            None => 1,
        };
        i += 1;
    }
    return s;
}
fn main() {
    let k = 2;
    let r = run(fn (int x) use (k) {
        if x % 3 == 0 {
            return None;
        }
        return Some(x / k);
    });
    let _ = write(stdout(), to_bytes(format("%i", r)));
}
"#;
    assert_eq!(run_example_src(src), "16");

    let mut pipeline = test_pipeline();
    pipeline.set_inline_max_cost(0);
    let compiled = compile(&mut pipeline, src).expect("pair-returning lambda");
    assert!(
        compiled.insts("run").any(|i| matches!(i, MirInst::CallIndirect { pair: Some(_), .. })),
        "`run` reads the result as two words"
    );
    let twins: Vec<_> = compiled.bodies.iter().filter_map(|b| b.mir.pair_twin).collect();
    assert_eq!(twins.len(), 1, "the lambda has a two-word twin");
    let twin = compiled.bodies.iter().find(|b| b.entry == Some(twins[0])).expect("the twin body");
    assert!(twin.mir.ret_hi_ty.is_some(), "the twin returns two words");
    assert!(
        !twin.mir.blocks.iter().flat_map(|b| &b.insts).any(|i| matches!(i, MirInst::Alloc { kind: MirAllocKind::Enum { .. }, .. })),
        "the twin builds no box"
    );
}
