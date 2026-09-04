//! End-to-end golden tests for `.hy` example programs.

use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

/// Absolute path to coil-stdlib `src/` (sibling clone or `.deps/` checkout).
fn workspace_stdlib() -> PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate parent");
    if let Ok(p) = std::env::var("COIL_STDLIB") {
        return PathBuf::from(p);
    }
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

use compiler::Pipeline;
use machine::Machine;

/// Captures VM `PRINT` output into a shared buffer (`Send` for thread workers).
#[derive(Clone, Default)]
struct SharedBuf {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl SharedBuf {
    pub fn new() -> Self {
        Self::default()
    }

    fn into_utf8(self) -> String {
        let bytes = self
            .inner
            .lock()
            .expect("print buffer mutex poisoned")
            .clone();
        String::from_utf8(bytes).expect("captured output should be valid UTF-8")
    }
}

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner
            .lock()
            .map_err(|_| std::io::ErrorKind::Other)?
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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
        peak >= 1 && peak <= EXAMPLE_POOL_SIZE,
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

fn assert_compile_fails_pipeline(pipeline: &mut Pipeline, src: &str, code: compiler::ErrorCode) {
    let result = pipeline.compile_src(src);
    let msgs: Vec<_> = pipeline
        .messages()
        .iter()
        .map(|m| (m.code(), m.message().to_string()))
        .collect();
    assert!(result.is_err(), "expected compile failure, got Ok; {msgs:?}");
    assert!(
        pipeline.messages().iter().any(|m| m.code() == Some(code)),
        "expected {code:?}, got {msgs:?}"
    );
}

fn compile_ok(pipeline: &mut Pipeline, src: &str) -> Vec<common::Byte> {
    pipeline
        .compile_src(src)
        .unwrap_or_else(|_| {
            panic!(
                "expected compile Ok, messages={:?}",
                pipeline
                    .messages()
                    .iter()
                    .map(|m| (m.code(), m.message().to_string()))
                    .collect::<Vec<_>>()
            )
        })
        .0
}

#[test]
fn granted_exec_emits_host_invoke() {
    let mut pipeline = test_pipeline();
    pipeline.grant_exec();
    let bc = compile_ok(
        &mut pipeline,
        r#"
use env::{exec};
fn main() {
    let args: Vec<string> = [];
    let _ = exec("true", args);
}
"#,
    );
    assert!(
        bc.iter()
            .any(|b| matches!(b.bytecode(), common::Instruction::HostInvoke)),
        "granted env::exec must emit HostInvoke, opcodes={:?}",
        bc.iter().map(|b| *b.bytecode()).collect::<Vec<_>>()
    );
}

#[test]
fn granted_exit_emits_host_invoke() {
    let mut pipeline = test_pipeline();
    pipeline.grant_exit();
    let bc = compile_ok(
        &mut pipeline,
        r#"
use env::{exit};
fn main() { exit(0); }
"#,
    );
    assert!(
        bc.iter()
            .any(|b| matches!(b.bytecode(), common::Instruction::HostInvoke)),
        "granted env::exit must emit HostInvoke"
    );
}

#[test]
fn granted_dload_emits_ffi_load() {
    let mut pipeline = test_pipeline();
    pipeline.grant_dload_allow("plugin");
    let bc = compile_ok(
        &mut pipeline,
        r#"
use ffi::{dload};
fn main() { let _ = dload("plugin"); }
"#,
    );
    assert!(
        bc.iter()
            .any(|b| matches!(b.bytecode(), common::Instruction::FfiLoad)),
        "granted dload must emit FfiLoad"
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

#[test]
fn dload_nonconst_is_compile_error() {
    let mut pipeline = test_pipeline();
    pipeline.grant_dload_allow("plugin");
    assert_compile_fails_pipeline(
        &mut pipeline,
        r#"
use ffi::{dload};
fn main() {
    let name = "plugin";
    let _ = dload(name);
}
"#,
        compiler::ErrorCode::HostDloadNonConst,
    );
}

fn test_pipeline() -> Pipeline {
    let mut p = Pipeline::new();
    p.bind_workspace_language_roots();
    p
}

fn compile_src_with_tests(src: &str) -> (Pipeline, Vec<common::Byte>, Vec<u64>) {
    let mut pipeline = test_pipeline();
    pipeline.set_include_tests(true);
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("source with tests should compile");
    (pipeline, bytecode, constants)
}

fn run_harness_src(src: &str) -> String {
    let (pipeline, bytecode, constants) = compile_src_with_tests(src);
    run_bytecode(bytecode, constants, &pipeline, None)
}

fn run_example_src_with_entry(src: &str, entry: Option<&std::path::Path>) -> String {
    let mut pipeline = test_pipeline();
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("example failed to compile (parse error or type errors)");
    run_bytecode(bytecode, constants, &pipeline, entry)
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
    let (bytecode, constants) = pipeline
        .compile_src_from_file(full.to_str().unwrap())
        .unwrap_or_else(|_| panic!("multi-file example failed to compile: {}", full.display()));
    run_bytecode(bytecode, constants, &pipeline, Some(full.as_path()))
}

/// Soft-skip an FFI-dependent test outside CI. In CI (`CI` env set), skip is a
/// hard failure so missing `cc` / libffi never silently greens the suite.
fn ffi_soft_skip(reason: &str) {
    if std::env::var_os("CI").is_some() {
        panic!("FFI soft-skip forbidden in CI: {reason}");
    }
    eprintln!("skipping: {reason}");
}

fn run_bytecode(
    bytecode: Vec<common::Byte>,
    constants: Vec<u64>,
    pipeline: &Pipeline,
    entry: Option<&std::path::Path>,
) -> String {
    let operand_slots = pipeline.operand_stack_slots() as usize;
    let shared = SharedBuf::new();
    let mut machine = Machine::<256>::with_operand_capacity(operand_slots);
    machine.set_shared_print(shared.inner.clone());
    machine.with_output(shared.clone());
    pipeline.wire_vm_ffi(&mut machine, entry);
    pipeline.wire_host_natives(&mut machine);
    pipeline.wire_thread_program(&mut machine, &bytecode, &constants, pipeline.strings());
    machine.set_program_debug(pipeline.program_debug());
    machine.run_raw(
        &bytecode,
        &constants,
        pipeline.strings(),
        pipeline.static_slot_count(),
    );
    let _ = machine.restore_output();
    shared.into_utf8()
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
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("example failed to compile (parse error or type errors)");
    run_bytecode(bytecode, constants, &pipeline, entry)
}

#[test]
fn example_panic_loc_archive_has_source_files() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let path = workspace_root.join("examples/panic_loc.hy");
    let mut pipeline = test_pipeline();
    let (bytecode, _constants) = pipeline
        .compile_src_from_file(path.to_str().unwrap())
        .expect("panic_loc should compile");
    let debug = pipeline.program_debug();
    assert!(
        debug
            .source_files
            .iter()
            .any(|p| p.contains("panic_loc.hy")),
        "expected panic_loc.hy in source_files: {:?}",
        debug.source_files
    );
    assert_eq!(debug.debug_locs.len(), bytecode.len());
    use common::Instruction;
    assert!(
        bytecode
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::Panic)),
        "expected Panic opcode"
    );
}

#[test]
fn example_option_prints_42() {
    let output = run_example("examples/option.hy");
    assert_eq!(output, "42");
}

#[test]
fn example_scalar_enum_prints_ok_200() {
    let output = run_example("examples/scalar_enum.hy");
    assert_eq!(output, "ok 200 200\n");
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
    let (bytecode, _) = pipeline.compile_src(src).expect("scalar enum should compile");
    assert!(
        !bytecode
            .iter()
            .any(|b| matches!(b.bytecode(), common::Instruction::MakeEnum)),
        "scalar-backed HttpCode::Ok must not allocate ObjEnum (MakeEnum)"
    );
}

#[test]
fn example_generics_uses_builtin_dictionary_abi() {
    let output = run_example("examples/generics.hy");
    assert_eq!(output, "7424.0427");
}

#[test]
fn example_result_prints_42_and_neg1() {
    let output = run_example("examples/result.hy");
    assert_eq!(output, "420-1");
}

#[test]
fn example_raise_try_prints_10_neg() {
    let output = run_example("examples/raise_try.hy");
    assert_eq!(output, "10,neg");
}

#[test]
fn example_assert_prints_ok_assertion_failed_custom() {
    let output = run_example("examples/assert.hy");
    assert_eq!(output, "ok,assertion failed,custom");
}

#[test]
fn panic_aborts_and_writes_message() {
    let mut pipeline = test_pipeline();
    let (bytecode, constants) = pipeline
        .compile_src(
            r#"
fn main() {
    panic "boom";
}
"#,
        )
        .expect("panic example should compile");
    let shared = SharedBuf::new();
    let mut machine = Machine::<128>::default();
    machine.with_output(shared.clone());
    machine.run_raw(
        &bytecode,
        &constants,
        pipeline.strings(),
        pipeline.static_slot_count(),
    );
    assert!(machine.panicked(), "expected language-level panic");
    let _ = machine.restore_output();
    let s = shared.into_utf8();
    assert_eq!(s, "panic: boom");
}

#[test]
fn example_coalesce_prints_bar_hi_7_9() {
    let output = run_example("examples/coalesce.hy");
    assert_eq!(output, "bar,hi,7,9");
}

#[test]
fn example_optional_chain_prints_42_0() {
    let output = run_example("examples/optional_chain.hy");
    assert_eq!(output, "42,0");
}

#[test]
fn example_tree_prints_6() {
    let output = run_example("examples/tree.hy");
    assert_eq!(output, "6");
}

#[test]
fn example_fib_still_works() {
    let output = run_example("examples/fib.hy");
    assert_eq!(output, "55");
}

#[test]
fn example_record_prints_169_5_12() {
    let output = run_example("examples/record.hy");
    assert_eq!(output, "169512");
}

#[test]
fn example_dict_prints_42_100_42() {
    let output = run_example("examples/dict.hy");
    assert_eq!(output, "4210042");
}

#[test]
fn example_array_grow_prints_len_first_and_last() {
    let output = run_example("examples/array_grow.hy");
    assert_eq!(output, "414");
}

#[test]
fn example_static_singleton_prints_121() {
    let output = run_example("examples/static_singleton.hy");
    assert_eq!(output, "121");
}

/// `static fn new(...)` / `Class::fresh()` alongside positional `new Class(...)`.
#[test]
fn example_static_ctor_prints_42_1_1() {
    let output = run_example("examples/static_ctor.hy");
    assert_eq!(output, "42,1,1");
}

#[test]
fn example_static_minimal_prints_11() {
    let output = run_example("examples/static_minimal.hy");
    assert_eq!(output, "11");
}

#[test]
fn example_readonly_seal_prints_322() {
    let output = run_example("examples/readonly_seal.hy");
    assert_eq!(output, "322");
}

/// `static const` is readable via LoadStatic; only reassignment is rejected.
#[test]
fn static_const_reads_via_load_static() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
static const VERSION = 42;

fn main() {
    write(stdout(), to_bytes(format("%i", VERSION)));
}
"#,
    );
    assert_eq!(output, "42");
}

/// Class `const` fields are readable after construction (mutation is blocked separately).
#[test]
fn const_class_field_reads_after_construction() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Point {
    pub const x: int,
    pub y: int,
}

fn main() {
    let p = new Point(7, 9);
    write(stdout(), to_bytes(format("%i", p.x)));
    write(stdout(), to_bytes(format("%i", p.y)));
}
"#,
    );
    assert_eq!(output, "79");
}

#[test]
fn example_classes_prints_7458() {
    let output = run_example("examples/classes.hy");
    assert_eq!(output, "7458");
}

#[test]
fn example_generic_class_prints_42() {
    let output = run_example("examples/generic_class.hy");
    assert_eq!(output, "42");
}

#[test]
fn example_aliases_prints_3_4_7() {
    let output = run_example("examples/aliases.hy");
    assert_eq!(output, "347");
}

#[test]
fn example_nested_aggregates_prints_rows_and_total() {
    let output = run_example("examples/nested_aggregates.hy");
    assert_eq!(output, "alice:30bob:25total:55");
}

#[test]
fn example_modules_brace_prints_12_42() {
    let output = run_example_multifile("examples/modules_brace.hy");
    assert_eq!(output, "1242");
}

#[test]
fn example_match_block_self_prints_5() {
    let output = run_example("examples/match_block_self.hy");
    assert_eq!(output, "5");
}

#[test]
fn example_defer_prints_enterleave_lifo_and_early_return() {
    let output = run_example("examples/defer.hy");
    assert_eq!(output, "enterleave,021,okd7,d99,55");
}

/// Regression: inherent `fn send` must not shadow `thread::send`, so
/// `send(self.channel, msg)` typechecks and runs.
#[test]
fn method_named_send_can_call_thread_send_on_self_channel() {
    let output = run_example_src(
        r#"
use thread::{channel, recv, send, spawn, Sender, Thread};
use io::{stdout, write};
use string::{format, to_bytes};

class ThreadWrapper {
    pub thread: Thread,
    pub channel: Sender,
}

impl ThreadWrapper {
    pub fn send(string msg) {
        send(self.channel, msg)?;
    }
}

fn work() -> int { return 0; }

fn main() {
    let pair = channel()?;
    let t = spawn(work)?;
    let w = new ThreadWrapper(t, pair[0]);
    w.send("hi")?;
    write(stdout(), to_bytes(format("%s", recv(pair[1])?)));
}
"#,
    );
    assert_eq!(output, "hi");
}

/// Regression: parenthesized `(self).field` must still emit GetField.
/// Pre-fix, `receiver_type` ignored `Group`, so Access fell through to
/// `LoadField(0)` and `send` received a bogus value (often looking like
/// the class instance was passed instead of the field).
#[test]
fn grouped_self_field_passes_sender_to_send() {
    let output = run_example_src(
        r#"
use thread::{channel, recv, send, Sender};
use io::{stdout, write};
use string::{format, to_bytes};

class Worker {
    pub tx: Sender,
}

impl Worker {
    pub fn push(string msg) {
        send((self).tx, msg)?;
    }
}

fn main() {
    let pair = channel()?;
    let w = new Worker(pair[0]);
    w.push("hi")?;
    write(stdout(), to_bytes(format("%s", recv(pair[1])?)));
}
"#,
    );
    assert_eq!(output, "hi");
}

/// Regression: field access on `new Class(...)` must use GetField, and
/// `new` must not leave a stash slot between a HostInvoke native-id and
/// the field value (StorePop+final LOAD used to bury the id so `send`
/// saw the instance address as the native id).
#[test]
fn inline_new_field_passes_sender_to_send() {
    let output = run_example_src(
        r#"
use thread::{channel, recv, send, Sender};
use io::{stdout, write};
use string::{format, to_bytes};

class Worker {
    pub tx: Sender,
}

fn main() {
    let pair = channel()?;
    send((new Worker(pair[0])).tx, "hi")?;
    write(stdout(), to_bytes(format("%s", recv(pair[1])?)));
}
"#,
    );
    assert_eq!(output, "hi");
}

/// Regression: method call on `(new Class(...))` must resolve the owner.
#[test]
fn inline_new_method_call_works() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn sum() -> int {
        return self.x + self.y;
    }
}

fn main() {
    write(stdout(), to_bytes(format("%i", (new Point(1, 3)).sum())));
}
"#,
    );
    assert_eq!(output, "4");
}

/// Regression: `defer` inside a function must run on early `return`, not
/// only on fall-through.
#[test]
fn defer_runs_on_early_return() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn f(int n) -> int {
    defer { write(stdout(), to_bytes("d")); }
    if n == 0 {
        return 1;
    }
    return 2;
}

fn main() {
    write(stdout(), to_bytes(format("%i,", f(0))));
    write(stdout(), to_bytes(format("%i", f(9))));
}
"#,
    );
    assert_eq!(output, "d1,d2");
}

#[test]
fn example_generic_alias_prints_7() {
    let output = run_example("examples/generic_alias.hy");
    assert_eq!(output, "7");
}

#[test]
fn example_generic_enum_prints_7() {
    let output = run_example("examples/generic_enum.hy");
    assert_eq!(output, "7");
}

#[test]
fn example_generics_prints_add_results_for_int_and_float() {
    let output = run_example("examples/generics.hy");
    assert_eq!(output, "7424.0427");
}

#[test]
fn example_typeclass_dict_forwards_dictionary_and_prints_42_twice() {
    let output = run_example("examples/typeclass_dict.hy");
    assert_eq!(output, "4242");
}

#[test]
fn example_typeclass_default_calls_sibling_and_prints_42() {
    let output = run_example("examples/typeclass_default.hy");
    assert_eq!(output, "42");
}

#[test]
fn example_polyfn_supports_multi_instantiation_constraints_and_rank_n() {
    let output = run_example("examples/polyfn.hy");
    assert_eq!(output, "424.0424242");
}

/// Phase 4: `%v` displays through Show (builtin + user instance + format).
#[test]
fn example_generic_print_shows_primitives_and_user_type() {
    let output = run_example("examples/generic_print.hy");
    assert_eq!(output, "42hi1.5true(3,4)99");
}

/// Advanced generics Phase 4: a bare unary trait name is an existential type.
#[test]
fn example_existential_show_prints_42() {
    let output = run_example("examples/existential_show.hy");
    assert_eq!(output, "42");
}

/// Phase 8: tuples and anonymous records have structural Show for `%v`.
#[test]
fn example_show_tuple_prints_structural_tuple_and_record() {
    let output = run_example("examples/show_tuple.hy");
    assert_eq!(output, "(1, 2){ a: 3, b: 4 }");
}

/// Constructor-kind trait `Container<Option>` + `get<F: Container, A>(F<A>)`.
#[test]
fn example_hkt_container_prints_42() {
    let output = run_example("examples/hkt_container.hy");
    assert_eq!(output, "42");
}

/// Phase 1 advanced generics: binary HKT `Bifunctor<Result>`.
#[test]
fn example_hkt_bifunctor_prints_42() {
    let output = run_example("examples/hkt_bifunctor.hy");
    assert_eq!(output, "42");
}

/// Phase 3: multi-param trait `Convert<A, B>` + `where` clause.
#[test]
fn example_multiparam_prints_42() {
    let output = run_example("examples/multiparam.hy");
    assert_eq!(output, "42");
}

/// Prelude `Into`: `let f: Fahrenheit = c.into();` with two local classes.
#[test]
fn example_into_prints_32() {
    let output = run_example("examples/into.hy");
    assert_eq!(output, "32");
}

/// Inline receiver `new Celsius(0).into()` must typecheck and run (Bugbot:
/// codegen used to skip boxing when `receiver_type` only handled Identifier/Access).
#[test]
fn inline_into_receiver_prints_32() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Celsius { pub c: int }
class Fahrenheit { pub f: int }
impl Into<Fahrenheit> for Celsius {
    fn into(Celsius x) -> Fahrenheit {
        return new Fahrenheit(x.c * 2 + 32);
    }
}
fn main() {
    let f: Fahrenheit = new Celsius(0).into();
    write(stdout(), to_bytes(format("%i", f.f)));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "32");
}

/// `return c.into();` under `-> Fahrenheit` pins the Into target at runtime.
#[test]
fn return_into_pins_target_prints_32() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Celsius { pub c: int }
class Fahrenheit { pub f: int }
class Kelvin { pub k: int }
impl Into<Fahrenheit> for Celsius {
    fn into(Celsius x) -> Fahrenheit {
        return new Fahrenheit(x.c * 2 + 32);
    }
}
impl Into<Kelvin> for Celsius {
    fn into(Celsius x) -> Kelvin {
        return new Kelvin(x.c);
    }
}
fn to_f(Celsius c) -> Fahrenheit {
    return c.into();
}
fn main() {
    let f = to_f(new Celsius(0));
    write(stdout(), to_bytes(format("%i", f.f)));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "32");
}

/// Phase 5: superclass / implied bounds (`Ordered<T: Equal>` → `eq_val` under `T: Ordered`).
#[test]
fn example_superclass_ord_prints_truetruefalse() {
    let output = run_example("examples/superclass_ord.hy");
    assert_eq!(output, "truetruefalse");
}

/// Advanced generics Phase 5: `c: * -> Constraint, T: c` with superclass method use.
#[test]
fn example_constraint_kind_prints_42() {
    let output = run_example("examples/constraint_kind.hy");
    assert_eq!(output, "42");
}

/// Phase 6: associated types — `Collect::Elem` pinned from ground instance.
#[test]
fn example_assoc_type_prints_42() {
    let output = run_example("examples/assoc_type.hy");
    assert_eq!(output, "42");
}

/// Phase 3 advanced generics: generic associated type `Pointer::Ref<A>`.
#[test]
fn example_gat_pointer_prints_42() {
    let output = run_example("examples/gat_pointer.hy");
    assert_eq!(output, "42");
}

/// Shuffled record pattern `{ y: _, x: a }` must bind declaration-order `x`.
#[test]
fn shuffled_record_pattern_binds_declaration_order_field() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        enum E { Foo { x: int, y: int, z: int } }
        fn main() {
            let e = E::Foo { x: 1, y: 2, z: 3 };
            let v = match e {
                E::Foo { y: _, x: a, z: _ } => a,
            };
            write(stdout(), to_bytes(format("%i", v)));
        }
    "#;
    assert_eq!(run_example_src(src), "1");
}

/// Phase 4: open `%v` inside a Show-bound generic body.
#[test]
fn generic_print_open_bound_uses_show_dictionary() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        fn show_it<T: Show>(T x) {
            write(stdout(), to_bytes(format("%v,", x)));
        }
        fn main() {
            show_it(10);
            show_it(20);
        }
    "#;
    assert_eq!(run_example_src(src), "10,20,");
}

/// Phase 4: `string::format("%v", ...)` produces a string for further use.
#[test]
fn format_percent_v_parity_with_print() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        fn main() {
            let s = format("%v-%v", 1, "x");
            write(stdout(), to_bytes(format("%s", s)));
        }
    "#;
    assert_eq!(run_example_src(src), "1-x");
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
            return describe_val(x);
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
    let (bytecode, _constants) = pipeline.compile_src(src).expect("compile");
    let capture = bytecode
        .iter()
        .find(|b| matches!(b.bytecode(), common::Instruction::MakePolyFnCapture))
        .expect("expected MakePolyFnCapture when escaping show from a constrained scope");
    assert_eq!(
        capture.operand_u32() & 0xFF,
        1,
        "capture should reserve one Describable dict slot"
    );
    // Application of the returned PolyFn must not require a second MakeTuple
    // dict at the call site — evidence lives in the capture.
    let capture_pos = bytecode
        .iter()
        .position(|b| matches!(b.bytecode(), common::Instruction::MakePolyFnCapture))
        .unwrap();
    let call_indirects_after: Vec<_> = bytecode[capture_pos..]
        .iter()
        .filter(|b| matches!(b.bytecode(), common::Instruction::CallIndirect))
        .collect();
    assert!(
        call_indirects_after
            .iter()
            .any(|b| (b.operand_u32() >> 16) == 0),
        "captured PolyFn call should use app_dict_arity=0; CallIndirect operands: {:?}",
        call_indirects_after
            .iter()
            .map(|b| b.operand_u32())
            .collect::<Vec<_>>()
    );
    assert_eq!(run_example_src(src), "42");
}

/// Inner-block PolyFn let must not poison an outer same-named ObjFn local.
#[test]
fn polyfn_block_shadow_does_not_box_outer_mono_fn() {
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
            return describe_val(x);
        }
        fn capture_show<T: Describable>(T _w) {
            return show;
        }
        fn add(int a, int b) -> int { return a + b; }
        fn main() {
            let f = add;
            {
                let f = capture_show(0);
                write(stdout(), to_bytes(format("%i", f(41))));
            }
            write(stdout(), to_bytes(format("%i", f(1, 2))));
        }
    "#;
    assert_eq!(run_example_src(src), "423");
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
            return cast(x);
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
    let (bytecode, _constants) = pipeline.compile_src(src).expect("compile");
    let capture = bytecode
        .iter()
        .find(|b| matches!(b.bytecode(), common::Instruction::MakePolyFnCapture))
        .expect("expected MakePolyFnCapture for multiparam escape");
    assert_eq!(capture.operand_u32() & 0xFF, 1);
    assert_eq!(run_example_src(src), "42");
}

/// Phase 1: PolyFn + fib-style arithmetic still receives peephole fusion.
#[test]
fn polyfn_with_fib_keeps_fused_superinstructions() {
    use common::Instruction;
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
    let (bytecode, constants) = pipeline.compile_src(src).expect("compile");
    assert!(
        bytecode
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::MakePolyFn))
    );
    // Fuse-select is intentionally conservative while absolute JMP joins
    // finish migrating; fib arithmetic may be unfused.
    let has_arith = bytecode.iter().any(|b| {
        matches!(
            *b.bytecode(),
            Instruction::ADD
                | Instruction::LEQ
                | Instruction::BinSlotImm
                | Instruction::BinSlotSlot
                | Instruction::BinReturn
        )
    });
    assert!(has_arith, "expected fib arithmetic with PolyFn present");
    let output = run_bytecode(bytecode, constants, &pipeline, None);
    // fib(6) = 8
    assert_eq!(output, "8");
}

#[test]
fn monomorphized_generic_add_prints_3() {
    let output = run_example_src(
        r#"use io::{stdout, write};
use string::{format, to_bytes};
fn add<T: Num>(T a, T b) -> T {
            return a + b;
        }

        fn main() {
            write(stdout(), to_bytes(format("%i", add(1, 2))));
        }"#,
    );
    assert_eq!(output, "3");
}

#[test]
fn example_const_prints_42hi() {
    let output = run_example("examples/const.hy");
    assert_eq!(output, "42hi");
}

#[test]
fn string_fmt_example_prints_concatenated_and_formatted_strings() {
    let output = run_example("examples/string_fmt.hy");
    assert_eq!(output, "hello world42-x");
}

#[test]
fn string_plus_equal_updates_binding() {
    let output = run_example_src(
        r#"use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
            let s = "a";
            s += "b";
            write(stdout(), to_bytes(format("%s", s)));
        }"#,
    );
    assert_eq!(output, "ab");
}

#[test]
fn example_mixed_prints_zero_circle_square_triangle() {
    let output = run_example("examples/mixed.hy");
    assert_eq!(output, "025122");
}

#[test]
fn example_chained_prints_42_7() {
    let output = run_example("examples/chained.hy");
    assert_eq!(output, "427");
}

#[test]
fn example_match_with_two_ok_arms_dispatches_correctly() {
    let output = run_example("examples/result.hy");
    assert_eq!(output, "420-1");
}

#[test]
fn fizbuz_runs_to_completion() {
    let _output = run_example("examples/fizbuz.hy");
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
    let (bytecode, constants) = pipeline.compile_test("", &mut ast);
    assert!(
        bytecode.is_empty(),
        "compile_test must not emit bytecode for a program the checker rejected"
    );
    assert!(constants.is_empty());
}

#[test]
fn let_binding_emits_store_pop_in_bytecode() {
    use common::Instruction;
    let mut pipeline = test_pipeline();
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        fn main() {
            let x = 5;
            write(stdout(), to_bytes(format("%i", x)));
            x = 10;
            write(stdout(), to_bytes(format("%i", x)));
        }
    "#;
    let parser = parser::Pratt::default();
    let mut ast = parser.parse(src).expect("let-binding program should parse");
    let (bytecode, _constants) = pipeline.compile_test("", &mut ast);
    assert!(!bytecode.is_empty(), "program should produce bytecode");

    let binding_store_count = bytecode
        .iter()
        .filter(|b| {
            matches!(b.bytecode(), Instruction::STORE) && b.load_store_single_slot() == Some(0)
        })
        .count();
    assert_eq!(
        binding_store_count, 2,
        "expected exactly 2 STORE writes to binding slot 0 for one let + one re-assignment; got {}",
        binding_store_count
    );
}

#[test]
fn example_let_reassignment_works() {
    let output = run_example("examples/let_test.hy");
    assert_eq!(output, "51020");
}

#[test]
fn example_named_args_prints_ada36_grace40() {
    let output = run_example("examples/named_args.hy");
    assert_eq!(output, "Ada36Grace40");
}

/// Critical regression: shuffled named args must reorder to declaration
/// order at runtime. Happy-path goldens only exercise source-order and
/// positional-prefix forms; a missing codegen reorder would still typecheck
/// here (string then int) but print the wrong values.
#[test]
fn named_args_shuffled_order_prints_correct_values() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn greet(string name, int age) {
    write(stdout(), to_bytes(format("%s", name)));
    write(stdout(), to_bytes(format("%i", age)));
}

fn main() {
    greet(age: 36, name: "Ada");
    greet(age: 40, name: "Grace");
}
"#,
    );
    assert_eq!(output, "Ada36Grace40");
}

#[test]
fn builtin_len_named_arg_prints_3() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    write(stdout(), to_bytes(format("%i", len(value: [1, 2, 3]))));
}
"#,
    );
    assert_eq!(output, "3");
}

#[test]
fn format_operand_string_concat_prints() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let n = 7;
    write(stdout(), to_bytes("n=" + format("%i", n)));
    write(stdout(), to_bytes(format("%i", n) + "!"));
}
"#,
    );
    assert_eq!(output, "n=77!");
}

#[test]
fn pure_arg_reorder_preserves_identifier_args() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn effect() -> int {
    write(stdout(), to_bytes(format("%i,", 7)));
    return 2;
}
fn sink(int a, int b) -> int {
    write(stdout(), to_bytes(format("%i,", a + b)));
    return a + b;
}
fn main() {
    let cached = 10;
    write(stdout(), to_bytes(format("%i", sink(effect(), cached))));
}
"#,
    );
    assert_eq!(output, "7,12,12");
}

/// Rest packing with a named fixed prefix + trailing positionals (P4 + P2).
#[test]
fn rest_after_named_fixed_prefix_packs_trailing() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn f(int a, int... xs) -> int {
    return a + len(xs);
}

fn main() {
    write(stdout(), to_bytes(format("%i", f(a: 10, 1, 2, 3))));
    write(stdout(), to_bytes(format("%i", f(a: 7))));
}
"#,
    );
    assert_eq!(output, "137");
}

#[test]
fn example_let_destructure_prints_12342() {
    let output = run_example("examples/let_destructure.hy");
    assert_eq!(output, "12342");
}

/// Nested let destructure must bind inner tuple slots correctly (not swap).
#[test]
fn let_nested_tuple_destructure_binds_in_order() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let (a, (b, c)) = (1, (2, 3));
    write(stdout(), to_bytes(format("%i", a)));
    write(stdout(), to_bytes(format("%i", b)));
    write(stdout(), to_bytes(format("%i", c)));
}
"#,
    );
    assert_eq!(output, "123");
}

#[test]
fn example_variadic_prints_60_hi() {
    let output = run_example("examples/variadic.hy");
    assert_eq!(output, "60Hi!?");
}

/// Phase P0: `let x = match { … }` must bind the arm value via
/// StorePop. Pre-fix Match emitted RETURN at end_label, so the
/// StorePop was unreachable and prints never ran / saw 0.
#[test]
fn let_match_binds_arm_value() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        fn main() {
            let x = match Option::None {
                Option::None => 7,
                Option::Some(v) => v,
            };
            write(stdout(), to_bytes(format("%i", x)));
            let y = match Option::Some(42) {
                Option::None => 0,
                Option::Some(v) => v,
            };
            write(stdout(), to_bytes(format("%i", y)));
        }
    "#;
    let output = run_example_src(src);
    assert_eq!(output, "742");
}

/// Phase P0: dict fields that hold heap objects (strings) must
/// round-trip through GetField.
#[test]
fn dict_string_field_round_trips() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        fn main() {
            let d = { name: "hi", n: 9 };
            write(stdout(), to_bytes(format("%s", d.name)));
            write(stdout(), to_bytes(format("%i", d.n)));
        }
    "#;
    let output = run_example_src(src);
    assert_eq!(output, "hi9");
}

/// Phase P1: in-place dict mutation via `d.field = value` then re-read.
#[test]
fn dict_mutation_round_trips() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        fn main() {
            let d = { foo: 1, name: "a" };
            d.foo = 99;
            d.name = "z";
            write(stdout(), to_bytes(format("%i", d.foo)));
            write(stdout(), to_bytes(format("%s", d.name)));
        }
    "#;
    let output = run_example_src(src);
    assert_eq!(output, "99z");
}

#[test]
fn example_let_chained_bindings_works() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate must have a parent (workspace root)");

    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        fn main() {
            let x = 5;
            let y = x + 1;
            write(stdout(), to_bytes(format("%i", y)));
        }
    "#;

    let mut pipeline = compiler::Pipeline::new();
    pipeline.bind_workspace_language_roots();
    let parser = parser::Pratt::default();
    let mut ast = parser
        .parse(src)
        .expect("chained-bindings program should parse");
    let (bytecode, constants) = pipeline.compile_test("", &mut ast);

    let shared = SharedBuf::new();
    let mut machine = machine::Machine::<128>::default();
    machine.with_output(shared.clone());
    pipeline.wire_host_natives(&mut machine);
    machine.run_raw(
        &bytecode,
        &constants,
        pipeline.strings(),
        pipeline.static_slot_count(),
    );

    let _ = machine.restore_output();
    let output = shared.into_utf8();

    let _ = workspace_root;
    assert_eq!(output, "6");
}

#[test]
fn nested_if_in_loop_runs_correctly() {
    let mut pipeline = compiler::Pipeline::new();
    pipeline.bind_workspace_language_roots();
    let src = r#"
        fn main() {
            let i = 0;
            while (i < 4) {
                if i < 2 { 1; }
                i = i + 1;
            }
        }
    "#;
    let parser = parser::Pratt::default();
    let mut ast = parser.parse(src).expect("nested if-in-loop should parse");
    let (bytecode, _constants) = pipeline.compile_test("", &mut ast);
    assert!(!bytecode.is_empty(), "program should produce bytecode");

    use common::Instruction;
    let exit_branch_count = bytecode
        .iter()
        .filter(|b| {
            matches!(
                b.bytecode(),
                Instruction::JMPF | Instruction::CmpJmpf | Instruction::BinSlotImmJmpf
            )
        })
        .count();
    let jmp_count = bytecode
        .iter()
        .filter(|b| matches!(b.bytecode(), Instruction::JMP))
        .count();
    assert!(
        exit_branch_count >= 2,
        "expected at least 2 exit branches (loop + if); got {}",
        exit_branch_count
    );
    assert!(
        jmp_count >= 1,
        "expected at least 1 JMP (loop back-edge); got {}",
        jmp_count
    );
}

#[test]
fn example_nested_records_prints_99() {
    let output = run_example("examples/nested_records.hy");
    assert_eq!(output, "99");
}

/// COI-16: inlined `Vec::push` must stage the receiver when the arg emits
/// `STORE`/`Seek` (`format`, `match`, `new Class`). Pre-fix, those pushes
/// silently dropped values (enum serialize via match+format yielded only the
/// header; `v.push(new C(...))` left len=0).
#[test]
fn vec_push_clobbering_args_preserve_elements() {
    let output = run_example_src(
        r#"
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

enum LockPackage {
    Git(string, string, string),
}

class Point {
    pub x: int,
    pub y: int,
}

fn quote(string s) -> string {
    return "'" + s + "'";
}

fn main() {
    let lines = Vec::new();
    lines.push(format("a=%s", "1"));
    lines.push(format("b=%s", "2"));
    let pkgs = Vec::new();
    pkgs.push(new Point(1, 2));
    pkgs.push(new Point(3, 4));
    let enums = Vec::new();
    enums.push(LockPackage::Git("n", "g", "t"));
    let e0 = enums[0];
    let names = Vec::new();
    names.push(match e0 {
        LockPackage::Git(name, git, tag) => format("name=%s", quote(name)),
    });
    let _ = write_all(
        stdout(),
        to_bytes(format(
            "lines=%i pkgs=%i names=%i n0=%s",
            len(lines),
            len(pkgs),
            len(names),
            names[0],
        )),
    );
}
"#,
    );
    assert_eq!(
        output, "lines=2 pkgs=2 names=1 n0=name='n'",
        "format / new Class / match+format push args must not drop the vec"
    );
}

/// Nested multi-field record patterns must not clobber sibling outer fields.
/// Pre-fix, in-place `UnpackAt` at the outer field slot overwrote later
/// siblings when the inner arity exceeded one.
#[test]
fn nested_multifield_record_pattern_preserves_sibling() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Inner {
    I { x: int, y: int },
}
enum Wrap {
    W { inner: Inner, name: int },
}
fn both(Wrap w) -> int {
    return match w {
        Wrap::W { inner: Inner::I { x, y }, name } => x + y + name,
    };
}
fn main() {
    let w = Wrap::W { inner: Inner::I { x: 10, y: 20 }, name: 3 };
    write(stdout(), to_bytes(format("%i", both(w))));
}
"#,
    );
    assert_eq!(
        output, "33",
        "inner fields and outer sibling `name` must all bind correctly"
    );
}

/// Two nested multifield records in one outer arm — each must unpack into
/// distinct scratch regions so the second nested payload cannot overwrite
/// the first nested bindings.
#[test]
fn nested_two_multifield_records_preserve_both() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Inner {
    I { x: int, y: int },
}
enum Pair {
    P { a: Inner, b: Inner },
}
fn sum(Pair p) -> int {
    return match p {
        Pair::P {
            a: Inner::I { x: ax, y: ay },
            b: Inner::I { x: bx, y: by },
        } => ax + ay + bx + by,
    };
}
fn main() {
    let p = Pair::P {
        a: Inner::I { x: 1, y: 2 },
        b: Inner::I { x: 10, y: 20 },
    };
    write(stdout(), to_bytes(format("%i", sum(p))));
}
"#,
    );
    assert_eq!(output, "33", "both nested multifield bindings must survive");
}

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
    let full = workspace_root.join("examples/ffi_sum.hy");
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

#[test]
fn example_strlen_is_compile_error_for_libc() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate must have a parent (workspace root)");
    let full = workspace_root.join("examples/strlen.hy");
    let src = std::fs::read_to_string(&full).expect("read strlen.hy");
    assert_compile_fails(&src, compiler::ErrorCode::HostDloadDenied);
}

#[test]
fn example_strlen_from_file_is_compile_error_for_libc() {
    let mut pipeline = test_pipeline();
    pipeline.grant_dload_stem("c");
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate must have a parent (workspace root)");
    let full = workspace_root.join("examples/strlen.hy");
    let result = pipeline.compile_src_from_file(full.to_str().unwrap());
    assert!(result.is_err());
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::HostDloadDenied))
    );
}

/// Serialize fd-1 redirection: parallel tests + libtest status lines share
/// process stdout, so nested `dup2` would corrupt capture.
#[cfg(unix)]
static OS_STDOUT_CAPTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Restores process stdout (fd 1) on drop — including panics inside `f`.
#[cfg(unix)]
struct StdoutFdGuard {
    old_stdout: i32,
}

#[cfg(unix)]
impl Drop for StdoutFdGuard {
    fn drop(&mut self) {
        if self.old_stdout < 0 {
            return;
        }
        unsafe {
            libc::fflush(std::ptr::null_mut());
            let _ = libc::dup2(self.old_stdout, 1);
            libc::close(self.old_stdout);
        }
        self.old_stdout = -1;
    }
}

/// Capture bytes written to OS stdout (fd 1) while `f` runs.
/// Needed for libc `printf`, which bypasses the VM's `PRINT` sink.
#[cfg(unix)]
fn with_captured_os_stdout<R>(f: impl FnOnce() -> R) -> (R, String) {
    use std::io::Read;
    use std::os::fd::FromRawFd;

    let _lock = OS_STDOUT_CAPTURE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let (read_fd, guard) = unsafe {
        let mut pipefd = [0i32; 2];
        assert_eq!(libc::pipe(pipefd.as_mut_ptr()), 0);
        let read_fd = pipefd[0];
        let write_fd = pipefd[1];
        let old_stdout = libc::dup(1);
        assert!(old_stdout >= 0);
        assert_eq!(libc::dup2(write_fd, 1), 1);
        libc::close(write_fd);
        (read_fd, StdoutFdGuard { old_stdout })
    };

    // Catch panics so we can restore fd 1 (via `guard`) before rethrowing —
    // otherwise later tests inherit a broken stdout.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    drop(guard);

    let mut file = unsafe { std::fs::File::from_raw_fd(read_fd) };
    let mut buf = Vec::new();
    let _ = file.read_to_end(&mut buf);
    drop(file);

    let s = String::from_utf8_lossy(&buf).into_owned();
    match result {
        Ok(r) => (r, s),
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Drop noise that lands in a process-wide stdout pipe under `cargo test`
/// parallelism: libtest harness status lines.
#[cfg(unix)]
fn clean_captured_os_stdout(output: &str) -> String {
    output
        .lines()
        .filter(|l| {
            let t = l.trim();
            if t.is_empty() {
                return false;
            }
            // libtest: `test foo::bar ... ok` (other threads finish mid-capture)
            if t.starts_with("test ") && t.contains(" ... ") {
                return false;
            }
            true
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(unix)]
#[test]
fn example_ffi_printf_is_compile_error_for_libc() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate must have a parent (workspace root)");
    let full = workspace_root.join("examples/ffi_printf.hy");
    let src = std::fs::read_to_string(&full).expect("read ffi_printf.hy");
    assert_compile_fails(&src, compiler::ErrorCode::HostDloadDenied);
}

#[test]
fn extern_missing_library_is_compile_error() {
    assert_compile_fails(
        r#"
extern "this_library_definitely_does_not_exist_xyzzy" {
    fn noop() -> int;
}
fn main() {
    let _ = noop();
}
"#,
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_missing_library_returns_err() {
    let extra = r#"
[dependencies]
time = { git = "https://example.com/coil-time.git", trusted = true }
"#;
    let output = run_userland_dload_project(
        "missing_time_trusted",
        extra,
        None,
        &dload_kind_program(&missing_abs_dload("time")),
        &["time"]);
    assert_eq!(
        output, "missing",
        "allow+trusted `time` must not be denied; got {output:?}"
    );
}

#[test]
fn userland_dload_unknown_stem_is_denied_not_missing() {
    assert_compile_fails(
        r#"
use ffi::{dload};
fn main() {
    let _ = dload("notalist");
}
"#,
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_c_is_denied() {
    assert_compile_fails(
        r#"
use ffi::{dload};
fn main() {
    let _ = dload("c");
}
"#,
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_absolute_non_allowlisted_is_denied() {
    let path = if cfg!(windows) {
        "C:/Windows/System32/kernel32.dll"
    } else {
        "/lib/x86_64-linux-gnu/libc.so.6"
    };
    let src = format!(
        r#"
use ffi::{{dload}};
fn main() {{
    let _ = dload("{path}");
}}
"#
    );
    assert_compile_fails(&src, compiler::ErrorCode::HostDloadDenied);
}

#[test]
fn userland_dload_first_party_stems_without_allow_are_denied() {
    let src = format!(
        r#"
use ffi::{{dload}};
fn main() {{
    let _ = dload("{}");
    let _ = dload("{}");
    let _ = dload("{}");
    let _ = dload("{}");
}}
"#,
        missing_abs_dload("time"),
        missing_abs_dload("crypto"),
        missing_abs_dload("tls"),
        missing_abs_dload("regex"),
    );
    assert_compile_fails(&src, compiler::ErrorCode::HostDloadDenied);
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

fn first_party_dep_line(stem: &str, trusted: Option<bool>) -> String {
    match trusted {
        Some(true) => format!(
            "{stem} = {{ git = \"https://example.com/coil-{stem}.git\", trusted = true }}\n"
        ),
        Some(false) => format!(
            "{stem} = {{ git = \"https://example.com/coil-{stem}.git\", trusted = false }}\n"
        ),
        None => format!("{stem} = {{ git = \"https://example.com/coil-{stem}.git\" }}\n"),
    }
}

fn dload_gate_for_project(
    test_name: &str,
    toml_extra: &str,
    lock: Option<&str>,
    dload_allow: &[&str],
) -> machine::DloadGate {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("coil_dload_gate_{test_name}_{pid}_{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir dload gate project");
    let stdlib = workspace_stdlib();
    std::fs::write(dir.join("coil.toml"), toml_extra).expect("write coil.toml");
    if let Some(lock) = lock {
        std::fs::write(dir.join("coil.lock"), lock).expect("write coil.lock");
    }
    let entry = dir.join("main.hy");
    std::fs::write(&entry, "fn main() {}\n").expect("write main.hy");
    let mut pipeline = Pipeline::new();
    pipeline.bind_project_roots_with_default(dir.clone(), [stdlib]);
    for stem in dload_allow {
        pipeline.grant_dload_allow(*stem);
    }
    pipeline
        .compile_src_from_file(entry.to_str().unwrap())
        .unwrap_or_else(|_| {
            for msg in pipeline.messages() {
                eprintln!("PIPELINE ERROR: {}", msg.message());
            }
            panic!("dload gate project failed to compile");
        });
    let gate = pipeline.build_dload_gate();
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

fn run_userland_dload_project(
    test_name: &str,
    toml_extra: &str,
    lock: Option<&str>,
    src: &str,
    dload_allow: &[&str],
) -> String {
    run_userland_dload_project_grants(
        test_name,
        toml_extra,
        lock,
        src,
        dload_allow,
        compiler::HostGrants::deny_all(),
    )
}

fn run_userland_dload_project_grants(
    test_name: &str,
    toml_extra: &str,
    lock: Option<&str>,
    src: &str,
    dload_allow: &[&str],
    mut grants: compiler::HostGrants,
) -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("coil_dload_{test_name}_{pid}_{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir dload project");
    let stdlib = workspace_stdlib();
    std::fs::write(dir.join("coil.toml"), toml_extra).expect("write coil.toml");
    if let Some(lock) = lock {
        std::fs::write(dir.join("coil.lock"), lock).expect("write coil.lock");
    }
    let entry = dir.join("main.hy");
    std::fs::write(&entry, src).expect("write main.hy");
    let mut pipeline = Pipeline::new();
    pipeline.bind_project_roots_with_default(dir.clone(), [stdlib]);
    for stem in dload_allow {
        grants.grant_dload_allow(*stem);
    }
    pipeline.set_host_grants(grants);
    let (bytecode, constants) = pipeline
        .compile_src_from_file(entry.to_str().unwrap())
        .unwrap_or_else(|_| {
            for msg in pipeline.messages() {
                eprintln!("PIPELINE ERROR: {}", msg.message());
            }
            panic!("dload project failed to compile");
        });
    let output = run_bytecode(bytecode, constants, &pipeline, Some(entry.as_path()));
    let _ = std::fs::remove_dir_all(&dir);
    output
}

fn assert_dload_project_compile_fails(
    test_name: &str,
    toml_extra: &str,
    lock: Option<&str>,
    src: &str,
    dload_allow: &[&str],
    code: compiler::ErrorCode,
) {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("coil_dload_fail_{test_name}_{pid}_{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir dload fail project");
    let stdlib = workspace_stdlib();
    std::fs::write(dir.join("coil.toml"), toml_extra).expect("write coil.toml");
    if let Some(lock) = lock {
        std::fs::write(dir.join("coil.lock"), lock).expect("write coil.lock");
    }
    let entry = dir.join("main.hy");
    std::fs::write(&entry, src).expect("write main.hy");
    let mut pipeline = Pipeline::new();
    pipeline.bind_project_roots_with_default(dir.clone(), [stdlib]);
    let mut grants = compiler::HostGrants::deny_all();
    for stem in dload_allow {
        grants.grant_dload_allow(*stem);
    }
    pipeline.set_host_grants(grants);
    let result = pipeline.compile_src_from_file(entry.to_str().unwrap());
    let msgs: Vec<_> = pipeline
        .messages()
        .iter()
        .map(|m| (m.code(), m.message().to_string()))
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(result.is_err(), "expected compile failure, got Ok; {msgs:?}");
    assert!(
        pipeline.messages().iter().any(|m| m.code() == Some(code)),
        "expected {code:?}, got {msgs:?}"
    );
}

#[test]
fn userland_dload_trusted_extra_without_pin_is_missing_not_denied() {
    let extra = r#"
[dependencies]
plugin = { git = "https://example.com/plugin.git", trusted = true }
"#;
    let output = run_userland_dload_project(
        "trusted_no_pin",
        extra,
        None,
        &dload_kind_program(&missing_abs_dload("plugin")),
        &["plugin"]);
    assert_eq!(output, "missing");
}

#[test]
fn userland_dload_trusted_extra_wrong_pin_is_missing_not_denied() {
    let extra = r#"
[dependencies]
plugin = { git = "https://example.com/plugin.git", trusted = true }
"#;
    let lock = "[[package]]
name = 'plugin'
[[package.native]]
sha256 = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
";
    let output = run_userland_dload_project(
        "trusted_wrong_pin",
        extra,
        Some(lock),
        &dload_kind_program(&missing_abs_dload("plugin")),
        &["plugin"]);
    assert_eq!(output, "missing");
}

#[test]
fn userland_dload_untrusted_extra_without_pin_is_denied() {
    let extra = r#"
[dependencies]
plugin = { git = "https://example.com/plugin.git", trusted = false }
"#;
    let output = run_userland_dload_project(
        "untrusted_no_pin",
        extra,
        None,
        &dload_kind_program(&missing_abs_dload("plugin")),
        &["plugin"]);
    assert_eq!(output, "denied");
}

#[test]
fn userland_dload_trusted_without_allow_is_denied() {
    let extra = r#"
[dependencies]
plugin = { git = "https://example.com/plugin.git", trusted = true }
"#;
    assert_dload_project_compile_fails(
        "trusted_no_allow",
        extra,
        None,
        &dload_kind_program(&missing_abs_dload("plugin")),
        &[],
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_toml_ffi_allow_is_ignored() {
    let extra = r#"
[ffi]
allow = ["plugin"]

[dependencies]
plugin = { git = "https://example.com/plugin.git", trusted = true }
"#;
    assert_dload_project_compile_fails(
        "toml_allow_ignored",
        extra,
        None,
        &dload_kind_program(&missing_abs_dload("plugin")),
        &[],
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_trusted_c_is_denied() {
    let extra = r#"
[dependencies]
c = { git = "https://example.com/libc.git", trusted = true }
"#;
    assert_dload_project_compile_fails(
        "trusted_c",
        extra,
        None,
        &dload_kind_program("c"),
        &[],
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_crypto_without_allow_is_denied() {
    let extra = r#"
[dependencies]
crypto = { git = "https://example.com/coil-crypto.git", trusted = true }
"#;
    assert_dload_project_compile_fails(
        "crypto_trusted_no_allow",
        extra,
        None,
        &dload_kind_program(&missing_abs_dload("crypto")),
        &[],
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_trusted_coil_prefixed_dep_maps_to_extra_stem() {
    let extra = r#"
[dependencies]
coil-plugin = { git = "https://example.com/plugin.git", trusted = true }
"#;
    let output = run_userland_dload_project(
        "trusted_coil_prefix",
        extra,
        None,
        &dload_kind_program(&missing_abs_dload("plugin")),
        &["plugin"]);
    assert_eq!(output, "missing");
}

#[test]
fn userland_dload_omitted_trusted_extra_without_pin_is_denied() {
    let extra = r#"
[dependencies]
plugin = { git = "https://example.com/plugin.git" }
"#;
    let output = run_userland_dload_project(
        "omitted_trusted_no_pin",
        extra,
        None,
        &dload_kind_program(&missing_abs_dload("plugin")),
        &["plugin"]);
    assert_eq!(output, "denied");
}

#[test]
fn userland_dload_trusted_libc_is_denied() {
    let extra = r#"
[dependencies]
libc = { git = "https://example.com/libc.git", trusted = true }
"#;
    assert_dload_project_compile_fails(
        "trusted_libc",
        extra,
        None,
        &dload_kind_program("libc"),
        &["libc"],
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_allowlisted_trusted_c_is_denied() {
    let extra = r#"
[dependencies]
c = { git = "https://example.com/libc.git", trusted = true }
"#;
    assert_dload_project_compile_fails(
        "trusted_allow_c",
        extra,
        None,
        &dload_kind_program("c"),
        &["c"],
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn userland_dload_crypto_allow_without_hash_or_trusted_is_denied() {
    let extra = r#"
[dependencies]
crypto = { git = "https://example.com/coil-crypto.git" }
"#;
    let output = run_userland_dload_project(
        "crypto_allow_no_hash",
        extra,
        None,
        &dload_kind_program(&missing_abs_dload("crypto")),
        &["crypto"]);
    assert_eq!(output, "denied");
}

#[test]
fn userland_dload_trusted_lock_native_stem_skips_hash() {
    let extra = r#"
[dependencies]
coil-http = { git = "https://example.com/http.git", trusted = true }
"#;
    let lock = "[[package]]
name = 'coil-http'
[[package.native]]
stem = 'plugin'
";
    let output = run_userland_dload_project(
        "trusted_lock_stem",
        extra,
        Some(lock),
        &dload_kind_program(&missing_abs_dload("plugin")),
        &["plugin"]);
    assert_eq!(output, "missing");
}

#[test]
fn userland_dload_bootstrap_crypto_allow_plus_trusted_is_missing() {
    let extra = r#"
[dependencies]
crypto = { git = "https://example.com/coil-crypto.git", trusted = true }
"#;
    let output = run_userland_dload_project(
        "bootstrap_crypto_trusted",
        extra,
        None,
        &dload_kind_program(&missing_abs_dload("crypto")),
        &["crypto"]);
    assert_eq!(output, "missing");
}

#[test]
fn userland_dload_crypto_allow_plus_lock_hash_is_missing() {
    let extra = r#"
[dependencies]
crypto = { git = "https://example.com/coil-crypto.git" }
"#;
    let lock = "[[package]]
name = 'crypto'
[[package.native]]
sha256 = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
";
    let output = run_userland_dload_project(
        "crypto_allow_lock_hash",
        extra,
        Some(lock),
        &dload_kind_program(&missing_abs_dload("crypto")),
        &["crypto"]);
    assert_eq!(output, "missing");
}

#[test]
fn pipeline_gate_trusted_extra_skips_native_hash() {
    let extra = r#"
[dependencies]
plugin = { git = "https://example.com/plugin.git", trusted = true }
"#;
    let gate = dload_gate_for_project("honor_skip_hash", extra, None, &["plugin"]);
    gate.check_request("plugin")
        .expect("trusted extra stem must pass");
    assert!(!gate.hash_required("plugin"));
}

#[test]
fn pipeline_gate_omitted_trusted_extra_requires_hash() {
    let extra = r#"
[dependencies]
plugin = { git = "https://example.com/plugin.git" }
"#;
    let gate = dload_gate_for_project("omitted_requires_hash", extra, None, &["plugin"]);
    assert_library_denied(&gate, "plugin", "plugin");
    assert!(gate.hash_required("plugin"));
}

#[test]
fn pipeline_gate_trusted_without_allow_still_loads() {
    let extra = r#"
[dependencies]
plugin = { git = "https://example.com/plugin.git", trusted = true }
"#;
    let gate = dload_gate_for_project("trusted_no_allow_gate", extra, None, &[]);
    gate.check_request("plugin")
        .expect("trusted is integrity, not a compile-time allow re-check");
}

#[test]
fn pipeline_gate_allowlisted_trusted_c_is_library_denied() {
    let extra = r#"
[dependencies]
c = { git = "https://example.com/libc.git", trusted = true }
plugin = { git = "https://example.com/plugin.git", trusted = true }
"#;
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        dload_gate_for_project("allow_trusted_c", extra, None, &["c", "plugin"])
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
    let extra = r#"
[dependencies]
plugin = { git = "https://example.com/plugin.git", trusted = true }
"#;
    let gate = dload_gate_for_project("first_party_no_allow", extra, None, &["plugin"]);
    for stem in machine::DLOAD_PRODUCTION_STEMS {
        assert_library_denied(&gate, stem, stem);
        assert!(
            gate.hash_required(stem),
            "{stem} must require hash unless trusted"
        );
    }
}

#[test]
fn pipeline_gate_crypto_allow_without_hash_or_trusted_is_denied() {
    let extra = r#"
[dependencies]
crypto = { git = "https://example.com/coil-crypto.git" }
"#;
    let gate = dload_gate_for_project("crypto_allow_no_hash_gate", extra, None, &["crypto"]);
    assert_library_denied(&gate, "crypto", "crypto");
    assert!(gate.hash_required("crypto"));
}

#[test]
fn pipeline_gate_bootstrap_crypto_allow_plus_trusted_skips_hash() {
    let extra = r#"
[dependencies]
crypto = { git = "https://example.com/coil-crypto.git", trusted = true }
"#;
    let gate = dload_gate_for_project("bootstrap_crypto_gate", extra, None, &["crypto"]);
    gate.check_request("crypto")
        .expect("bootstrap crypto allow+trusted must pass");
    assert!(!gate.hash_required("crypto"));
}

#[test]
fn userland_dload_first_party_trusted_without_allow_is_denied() {
    for stem in machine::DLOAD_PRODUCTION_STEMS {
        let extra = format!("[dependencies]\n{}", first_party_dep_line(stem, Some(true)));
        assert_dload_project_compile_fails(
            &format!("{stem}_trusted_no_allow"),
            &extra,
            None,
            &dload_kind_program(&missing_abs_dload(stem)),
            &[],
            compiler::ErrorCode::HostDloadDenied,
        );
    }
}

#[test]
fn userland_dload_first_party_allow_plus_trusted_is_missing() {
    for stem in machine::DLOAD_PRODUCTION_STEMS {
        let extra = format!(
            "[dependencies]\n{}",
            first_party_dep_line(stem, Some(true))
        );
        let output = run_userland_dload_project(
            &format!("{stem}_allow_trusted"),
            &extra,
            None,
            &dload_kind_program(&missing_abs_dload(stem)),
            &[stem]);
        assert_eq!(
            output, "missing",
            "{stem} allow+trusted must skip hash; missing file is LibraryNotFound"
        );
    }
}

#[test]
fn userland_dload_first_party_allow_without_hash_or_trusted_is_denied() {
    for stem in machine::DLOAD_PRODUCTION_STEMS {
        let extra = format!(
            "[dependencies]\n{}",
            first_party_dep_line(stem, None)
        );
        let output = run_userland_dload_project(
            &format!("{stem}_allow_omitted_trusted"),
            &extra,
            None,
            &dload_kind_program(&missing_abs_dload(stem)),
            &[stem]);
        assert_eq!(
            output, "denied",
            "{stem} allow without trusted and without pin must be denied"
        );
    }
}

#[test]
fn userland_dload_first_party_allow_trusted_false_without_pin_is_denied() {
    for stem in machine::DLOAD_PRODUCTION_STEMS {
        let extra = format!(
            "[dependencies]\n{}",
            first_party_dep_line(stem, Some(false))
        );
        let output = run_userland_dload_project(
            &format!("{stem}_allow_trusted_false"),
            &extra,
            None,
            &dload_kind_program(&missing_abs_dload(stem)),
            &[stem]);
        assert_eq!(
            output, "denied",
            "{stem} trusted = false must not skip native sha256"
        );
    }
}

#[test]
fn userland_dload_bootstrap_coil_crypto_trusted_is_missing() {
    let extra = r#"
[dependencies]
coil-crypto = { git = "https://example.com/coil-crypto.git", trusted = true }
"#;
    let output = run_userland_dload_project(
        "bootstrap_coil_crypto",
        extra,
        None,
        &dload_kind_program(&missing_abs_dload("crypto")),
        &["crypto"]);
    assert_eq!(output, "missing");
}

#[test]
fn userland_dload_allowlisted_trusted_libc_is_denied() {
    let extra = r#"
[dependencies]
libc = { git = "https://example.com/libc.git", trusted = true }
"#;
    assert_dload_project_compile_fails(
        "trusted_allow_libc",
        extra,
        None,
        &dload_kind_program("libc"),
        &["libc"],
        compiler::ErrorCode::HostDloadDenied,
    );
}

#[test]
fn pipeline_gate_first_party_allow_without_hash_or_trusted_is_denied() {
    for stem in machine::DLOAD_PRODUCTION_STEMS {
        let extra = format!(
            "[dependencies]\n{}",
            first_party_dep_line(stem, None)
        );
        let gate = dload_gate_for_project(&format!("{stem}_gate_no_hash"), &extra, None, &[stem]);
        assert_library_denied(&gate, stem, stem);
        assert!(gate.hash_required(stem), "{stem} must require a lock hash");
    }
}

#[test]
fn pipeline_gate_first_party_allow_plus_trusted_skips_hash() {
    for stem in machine::DLOAD_PRODUCTION_STEMS {
        let extra = format!(
            "[dependencies]\n{}",
            first_party_dep_line(stem, Some(true))
        );
        let gate = dload_gate_for_project(&format!("{stem}_gate_trusted"), &extra, None, &[stem]);
        gate.check_request(stem)
            .unwrap_or_else(|e| panic!("{stem} allow+trusted must pass, got {e:?}"));
        assert!(!gate.hash_required(stem), "{stem} trusted must skip hash");
    }
}

#[test]
fn example_coro_prints_suspended_1_resumed() {
    let output = run_example("examples/coro.hy");
    assert_eq!(output, "Suspended\n1Resumed\n");
}

#[test]
fn example_coro_gen_prints_012() {
    let output = run_example("examples/coro_gen.hy");
    assert_eq!(output, "012");
}

#[test]
fn example_coro_interleave_prints_out_of_order_counters() {
    let output = run_example("examples/coro_interleave.hy");
    assert_eq!(output, "10,100,101,11,12,102");
}

#[test]
fn example_coro_send_prints_hello() {
    let output = run_example("examples/coro_send.hy");
    assert_eq!(output, "hello");
}

#[test]
fn example_coro_yield_from_prints_012() {
    let output = run_example("examples/coro_yield_from.hy");
    assert_eq!(output, "012");
}

#[test]
fn example_coro_done_prints_false_false_true() {
    let output = run_example("examples/coro_done.hy");
    assert_eq!(output, "falsefalsetrue");
}

#[test]
fn example_for_in_coro_prints_012_and_breaks() {
    // counter yields 0,1,2 then returns 99 — completion must NOT print.
    // early yields 10,20,30 — break on 20 prints only 10.
    let output = run_example("examples/for_in_coro.hy");
    assert_eq!(output, "01210");
}

#[test]
fn example_for_in_array_prints_123() {
    let output = run_example("examples/for_in_array.hy");
    assert_eq!(output, "123");
}

#[test]
fn example_for_in_tuple_prints_123() {
    let output = run_example("examples/for_in_tuple.hy");
    assert_eq!(output, "123");
}

#[test]
fn example_for_in_dict_prints_12() {
    let output = run_example("examples/for_in_dict.hy");
    assert_eq!(output, "12");
}

#[test]
fn example_for_in_custom_prints_012() {
    let output = run_example("examples/for_in_custom.hy");
    assert_eq!(output, "012");
}

/// Impl methods are defined after the `for` user; CALL must not pack PC 0.
#[test]
fn for_in_impl_after_loop_still_iterates() {
    let output = run_example("tests/positive/for_in_impl_after.hy");
    assert_eq!(output, "3");
}

#[test]
fn example_range_prints_01234012356() {
    // 0..5 → 01234; 0..=3 → 0123; 10..0 empty; byte 5..=6 → 56;
    // float 1.0..4.0 → 1.02.03.0
    let output = run_example("examples/range.hy");
    assert_eq!(output, "012340123561.02.03.0");
}

/// Inner-block destructure must not clobber an outer binding's slot.
#[test]
fn let_destructure_block_shadow_preserves_outer_binding() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum A { A { z: int, x: int }, }
enum B { B { y: int }, }
fn main() {
  let outer = { a: A::A { z: 10, x: 42 } };
  let { a } = outer;
  { let inner = { a: B::B { y: 7 } }; let { a } = inner; }
  write(stdout(), to_bytes(format("%i", a.x)));
}
"#,
    );
    assert_eq!(output, "42", "outer `a` must survive inner-block shadow");
}

/// Rest-only generic with a typeclass constraint needs a call-site dict.
#[test]
fn generic_rest_only_show_call_emits_dict_and_prints() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn show_all<T: Show>(T... xs) {
    write(stdout(), to_bytes(format("%v", xs[0])));
}
fn main() {
    show_all(1);
}
"#,
    );
    assert_eq!(output, "1");
}

/// Rest-only Num generic must monomorphize (not print a boxed heap pointer).
#[test]
fn generic_rest_only_num_call_monomorphizes_and_prints() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn twice_first<T: Num>(T... xs) -> T { return xs[0] + xs[0]; }
fn main() { write(stdout(), to_bytes(format("%i", twice_first(21)))); }
"#,
    );
    assert_eq!(output, "42");
}

/// Shadowing a function parameter inside a block must restore Access typing.
#[test]
fn block_shadow_of_param_restores_access_field_type() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum A { A { z: int, x: int }, }
enum B { B { y: int }, }
fn foo(A a) {
  { let a = B::B { y: 7 }; }
  write(stdout(), to_bytes(format("%i", a.x)));
}
fn main() { foo(A::A { z: 10, x: 42 }); }
"#,
    );
    assert_eq!(output, "42");
}

/// Half-open vs inclusive endpoints: `0..1` yields only 0; `0..=1` yields 0,1.
#[test]
fn range_half_open_excludes_end_inclusive_includes_end() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    for x in 0..1 {
        write(stdout(), to_bytes(format("%i", x)));
    }
    write(stdout(), to_bytes(","));
    for x in 0..=1 {
        write(stdout(), to_bytes(format("%i", x)));
    }
}
"#,
    );
    assert_eq!(output, "0,01");
}

#[test]
fn range_to_vec_collects_int_byte_float() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let a = (0..5).to_vec();
    write(stdout(), to_bytes(format("%i", a.len())));
    write(stdout(), to_bytes(","));
    let r = 0..=3;
    write(stdout(), to_bytes(format("%i", r.to_vec().len())));
    write(stdout(), to_bytes(","));
    write(stdout(), to_bytes(format("%i", (10..0).to_vec().len())));
    write(stdout(), to_bytes(","));
    let lo: byte = 5;
    let hi: byte = 6;
    write(stdout(), to_bytes(format("%i", (lo..=hi).to_vec().len())));
    write(stdout(), to_bytes(","));
    write(stdout(), to_bytes(format("%i", (1.0..4.0).to_vec().len())));
}
"#,
    );
    // 0..5 → 5; 0..=3 → 4; 10..0 → 0; byte 5..=6 → 2; float 1.0..4.0 → 3
    assert_eq!(output, "5,4,0,2,3");
}

/// `.to_vec()` must yield the same sequence as `for` (shared LE/LEQ + step).
#[test]
fn range_to_vec_matches_for_in_elements() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    for x in 0..=3 {
        write(stdout(), to_bytes(format("%i", x)));
    }
    write(stdout(), to_bytes("|"));
    let v = (0..=3).to_vec();
    let i = 0;
    while i < v.len() {
        write(stdout(), to_bytes(format("%i", v[i])));
        i = i + 1;
    }
    write(stdout(), to_bytes("|"));
    write(stdout(), to_bytes(format("%i", (7..=7).to_vec()[0])));
    write(stdout(), to_bytes("|"));
    for x in 1.0..=3.0 {
        write(stdout(), to_bytes(format("%f", x)));
    }
    write(stdout(), to_bytes("|"));
    let f = (1.0..=3.0).to_vec();
    let j = 0;
    while j < f.len() {
        write(stdout(), to_bytes(format("%f", f[j])));
        j = j + 1;
    }
    write(stdout(), to_bytes("|"));
    write(stdout(), to_bytes(format("%i", (4.0..1.0).to_vec().len())));
}
"#,
    );
    assert_eq!(output, "0123|0123|7|1.02.03.0|1.02.03.0|0");
}

/// Int `to_vec` thunks fuse ADD/LE(Q); float sibling thunks keep ADDF and fuse LE(Q)F.
#[test]
fn range_to_vec_thunks_use_int_vs_float_opcodes() {
    let src = r#"
fn main() {
    let _a = (0..3).to_vec();
    let _b = (0..=2).to_vec();
    let _c = (1.0..3.0).to_vec();
    let _d = (1.0..=2.0).to_vec();
}
"#;
    let mut pipeline = test_pipeline();
    let (bytecode, _) = pipeline.compile_src(src).expect("range to_vec compile");
    let syms = pipeline.program_debug().fn_symbols;
    let body = |name: &str| {
        let idx = syms.iter().position(|s| s.name == name).unwrap_or_else(|| {
            panic!(
                "missing `{name}`; have {:?}",
                syms.iter().map(|s| &s.name).collect::<Vec<_>>()
            )
        });
        let start = syms[idx].entry_pc as usize;
        let end = syms
            .get(idx + 1)
            .map(|s| s.entry_pc as usize)
            .unwrap_or(bytecode.len());
        &bytecode[start..end]
    };
    let jmpf_ops = |slice: &[common::Byte]| -> Vec<u8> {
        slice
            .iter()
            .filter(|b| *b.bytecode() == common::Instruction::BinSlotSlotJmpf)
            .map(|b| b.bin_slot_slot_jmpf_parts().0)
            .collect()
    };
    let imm_store_ops = |slice: &[common::Byte]| -> Vec<u8> {
        slice
            .iter()
            .filter(|b| *b.bytecode() == common::Instruction::BinSlotImmStore)
            .map(|b| b.bin_slot_imm_store_parts().0)
            .collect()
    };
    let has =
        |slice: &[common::Byte], op: common::Instruction| slice.iter().any(|b| *b.bytecode() == op);

    let int_half = body("Range::to_vec");
    assert_eq!(
        jmpf_ops(int_half),
        vec![common::Instruction::LE as u8],
        "int half-open compare must be LE"
    );
    assert_eq!(
        imm_store_ops(int_half),
        vec![common::Instruction::ADD as u8],
        "int half-open step must be ADD"
    );
    assert!(!has(int_half, common::Instruction::ADDF));

    let int_inc = body("RangeInclusive::to_vec");
    assert_eq!(
        jmpf_ops(int_inc),
        vec![common::Instruction::LEQ as u8],
        "int inclusive compare must be LEQ"
    );
    assert_eq!(
        imm_store_ops(int_inc),
        vec![common::Instruction::ADD as u8],
        "int inclusive step must be ADD"
    );

    let float_half = body("Range::__float_to_vec");
    assert_eq!(
        jmpf_ops(float_half),
        vec![common::Instruction::LEF as u8],
        "float half-open compare must be LEF"
    );
    assert!(
        has(float_half, common::Instruction::ADDF),
        "float half-open step must use ADDF"
    );
    assert!(
        imm_store_ops(float_half).is_empty(),
        "float half-open must not fuse int BinSlotImmStore"
    );

    let float_inc = body("RangeInclusive::__float_to_vec");
    assert_eq!(
        jmpf_ops(float_inc),
        vec![common::Instruction::LEQF as u8],
        "float inclusive compare must be LEQF"
    );
    assert!(
        has(float_inc, common::Instruction::ADDF),
        "float inclusive step must use ADDF"
    );
}

/// Regression guard: `resume h` used INLINE as a `print` argument
/// (no intermediate `let` binding) must not corrupt the operand
/// stack. Pre-fix, the bare `yield expr;` statement's spurious
/// trailing `POP` (see `bare_yield_statement_does_not_emit_trailing_pop`
/// in `compiler/src/lib.rs`) would pop whatever the resumer had
/// already pushed for the in-progress `print` call (e.g. the format
/// string), leading to a misaligned pointer dereference.
#[test]
fn inline_resume_in_print_does_not_corrupt_stack() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        async fn counter() {
            yield 0;
            yield 1;
            yield 2;
        }

        fn main() {
            let h = counter();
            write(stdout(), to_bytes(format("%i,", resume h)));
            write(stdout(), to_bytes(format("%i,", resume h)));
            write(stdout(), to_bytes(format("%i", resume h)));
        }
    "#;
    let output = run_example_src(src);
    assert_eq!(output, "0,1,2");
}

/// Regression guard: two handles from the SAME parameterized
/// `async fn`, interleaved, with `resume` used inline. Pre-fix, the
/// same spurious trailing `POP` corrupted each coroutine's argument
/// slot (`base`) on every resume after the first yield, producing
/// wrong values once interleaving pushed other locals onto the
/// shared stack.
#[test]
fn parameterized_interleaved_coroutines_inline_resume_stay_independent() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        async fn counter(int base) {
            yield base;
            yield base + 1;
            yield base + 2;
        }

        fn main() {
            let a = counter(1);
            let b = counter(100);

            write(stdout(), to_bytes(format("%i,", resume a)));
            write(stdout(), to_bytes(format("%i,", resume b)));
            write(stdout(), to_bytes(format("%i,", resume a)));
            write(stdout(), to_bytes(format("%i,", resume b)));
            write(stdout(), to_bytes(format("%i,", resume a)));
            write(stdout(), to_bytes(format("%i", resume b)));
        }
    "#;
    let output = run_example_src(src);
    assert_eq!(output, "1,100,2,101,3,102");
}

/// `return e;` inside an `async fn` now produces a real completion
/// value (previously it was silently unified against `unit` and the
/// typechecker rejected any non-unit `return`). The value returned by
/// `return` propagates to the `resume` call that completes the
/// coroutine, exactly like a yielded value.
#[test]
fn coroutine_return_value_propagates_to_resume() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        async fn counter() {
            yield 1;
            yield 2;
            return 42;
        }

        fn main() {
            let h = counter();
            write(stdout(), to_bytes(format("%i,", resume h))); // yield 1
            write(stdout(), to_bytes(format("%i,", resume h))); // yield 2
            write(stdout(), to_bytes(format("%i", resume h)));  // return 42
        }
    "#;
    let output = run_example_src(src);
    assert_eq!(output, "1,2,42");
}

/// WP-M6: resuming an already-Done coroutine panics instead of returning a
/// stale sentinel value.
#[test]
fn resume_after_done_panics() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        async fn counter() {
            return 42;
        }

        fn main() {
            let h = counter();
            write(stdout(), to_bytes(format("%i,", resume h))); // return 42 (completes)
            write(stdout(), to_bytes(format("%i,", resume h))); // panics
            write(stdout(), to_bytes(format("%i", resume h)));
        }
    "#;
    let output = run_example_src(src);
    assert!(
        output.starts_with("42,"),
        "expected first resume output before panic, got: {output:?}"
    );
    assert!(
        output.contains("panic:") && output.contains("resumed after completion"),
        "expected resume-after-done panic, got: {output:?}"
    );
}

/// `done(h)` is false before the first resume and true after completion
/// without needing another resume (M6: extra resume panics).
#[test]
fn done_before_and_after_coroutine_completion() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        async fn counter() {
            yield 1;
            return 2;
        }

        fn main() {
            let h = counter();
            write(stdout(), to_bytes(format("%z,", done(h))));
            write(stdout(), to_bytes(format("%i,", resume h)));
            write(stdout(), to_bytes(format("%z,", done(h))));
            write(stdout(), to_bytes(format("%i,", resume h)));
            write(stdout(), to_bytes(format("%z", done(h))));
        }
    "#;
    let output = run_example_src(src);
    assert_eq!(output, "false,1,false,2,true");
}

/// Immediate-return async fn completes on first resume; `done` flips after.
#[test]
fn immediate_return_async_done_after_first_resume() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        async fn unit_ret() {
            return;
        }

        fn main() {
            let h = unit_ret();
            write(stdout(), to_bytes(format("%z,", done(h))));
            let _ = resume h;
            write(stdout(), to_bytes(format("%z", done(h))));
        }
    "#;
    let output = run_example_src(src);
    assert_eq!(output, "false,true");
}

/// `resume h with v` after completion panics (same M6 rule as bare resume).
#[test]
fn resume_with_send_after_done_panics() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        async fn sink() {
            let _ = yield 0;
            return 1;
        }

        fn main() {
            let h = sink();
            let _ = resume h;
            let _ = resume h with 10;
            let _ = resume h;
            write(stdout(), to_bytes(format("%i", resume h with 99)));
        }
    "#;
    let output = run_example_src(src);
    assert!(
        output.contains("panic:") && output.contains("resumed after completion"),
        "expected resume-with-send after done to panic, got: {output:?}"
    );
}

/// Inline `yield from` must delegate every yielded value before completing.
#[test]
fn yield_from_inline_delegates_all_values() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        async fn inner() {
            yield 1;
            yield 2;
        }
        async fn outer() {
            yield from inner();
        }

        fn main() {
            let h = outer();
            write(stdout(), to_bytes(format("%i,", resume h)));
            write(stdout(), to_bytes(format("%i,", resume h)));
            write(stdout(), to_bytes(format("%i", resume h)));
        }
    "#;
    let output = run_example_src(src);
    assert_eq!(output, "1,2,0");
}

/// `write_all` between resumes must not mark the delegating coroutine done.
#[test]
fn yield_from_write_all_between_resumes() {
    let src = r#"
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
        async fn counter() {
            yield 0;
            yield 1;
            yield 2;
        }
        async fn wrap() {
            yield from counter();
        }

        fn main() {
            let h = wrap();
            let v0 = resume h;
            write_all(stdout(), to_bytes(format("%i", v0)));
            let v1 = resume h;
            write_all(stdout(), to_bytes(format("%i", v1)));
            let v2 = resume h;
            write_all(stdout(), to_bytes(format("%i", v2)));
        }
    "#;
    let output = run_example_src(src);
    assert_eq!(output, "012");
}

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
    let output = run_ffi_example_with_lib("examples/ffi_array.hy", &libsum);
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
    let output = run_ffi_example_with_lib("examples/ffi_callback.hy", &libsum);
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
    let output = run_ffi_example_with_lib("examples/ffi_struct_ret.hy", &libsum);
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
    let output = run_ffi_example_with_lib("examples/ffi_callback_ret.hy", &libsum);
    assert_eq!(output, "1");
}

#[test]
fn example_operators_prints_expected() {
    let output = run_example("examples/operators.hy");
    assert_eq!(output, "801125428falsetrue3");
}

#[test]
fn example_while_loop_accumulates_correctly() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
        fn main() {
            let acc = 0;
            let i = 0;
            while (i < 100) {
                acc = acc + i;
                i = i + 1;
            }
            write(stdout(), to_bytes(format("%i", acc)));
        }
        "#,
    );
    assert_eq!(output, "4950");
}

#[test]
fn example_for_break_prints_18() {
    let output = run_example("examples/for_break.hy");
    assert_eq!(output, "18");
}

#[test]
fn example_derive_show_eq_prints_expected() {
    let output = run_example("examples/derive_show_eq.hy");
    assert_eq!(
        output,
        "Color::Red,true,false,true,Point::Point { x: 5, y: 12 },true,false,Cell { value: 42 },true,false"
    );
}

#[test]
fn example_typeof_len_prints_expected() {
    let output = run_example("examples/typeof_len.hy");
    assert_eq!(
        output,
        "int\nstring\n(int, int)\n3\n3\n2\n2\nPoint\nPoint\n"
    );
}

#[test]
fn example_length_trait_prints_expected() {
    let output = run_example("examples/length_trait.hy");
    assert_eq!(output, "3\n2\n42\n");
}

/// Structural `len` on non-literal values: string hits VM `ArrayLen`;
/// tuple/dict use typed static length after binding (not literal fold).
#[test]
fn runtime_len_of_string_tuple_dict_params() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};

fn id_str(string s) -> string { return s; }

fn main() {
    let t = (1, 2, 3);
    let d = { a: 1, b: 2 };
    write(stdout(), to_bytes(format("%i\n", len(id_str("ab")))));
    write(stdout(), to_bytes(format("%i\n", len(t))));
    write(stdout(), to_bytes(format("%i\n", len(d))));
}
"#,
    );
    assert_eq!(output, "2\n3\n2\n");
}

/// `typeof` of prelude Option/Result must print the module-qualified FQN.
#[test]
fn typeof_option_prints_prelude_fqn() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let o = Option::Some(1);
    let r: Result<int, string> = Result::Ok(1);
    write(stdout(), to_bytes(format("%s\n", typeof o)));
    write(stdout(), to_bytes(format("%s\n", typeof r)));
}
"#,
    );
    assert_eq!(
        output,
        "prelude::Option<int>\nprelude::Result<int, string>\n"
    );
}

/// Regression: derived `Serialize::serialize` must typecheck (`[byte]` return,
/// payload fields cast to `byte`).
#[test]
fn derive_serialize_enum_e2e_typechecks() {
    let src = r#"
#[derive(Serialize)]
enum E {
    A,
    B(int),
}

fn main() {}
"#;
    let mut pipeline = test_pipeline();
    pipeline
        .compile_src(src)
        .expect("derive Serialize should compile without type errors");
}

/// Regression: concrete `<`/`>` codegen must look up `Lt`/`Gt` (not empty
/// `Ord`), otherwise unit-enum compares fall back to raw heap-pointer `LE`
/// and become ASLR-flaky (`Red < Blue` randomly false).
#[test]
fn derive_ord_unit_variants_compare_by_declaration_order() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
#[derive(Ord)]
enum Color {
    Red,
    Blue,
}

fn main() {
    write(stdout(), to_bytes(format("%z,", Color::Red < Color::Blue)));
    write(stdout(), to_bytes(format("%z,", Color::Blue < Color::Red)));
    write(stdout(), to_bytes(format("%z,", Color::Red < Color::Red)));
    write(stdout(), to_bytes(format("%z,", Color::Red <= Color::Red)));
    write(stdout(), to_bytes(format("%z", Color::Blue > Color::Red)));
}
"#;
    for _ in 0..8 {
        let output = run_example_src(src);
        assert_eq!(
            output, "true,false,false,true,true",
            "unit-enum Ord must be tag-order stable (not pointer order)"
        );
    }
}

/// Regression: `derive Ord` must emit Lt/Le/Gt/Ge + empty Ord (PR #14 layout)
/// and lexicographic field compare must use strict `<` so equal prefixes fall
/// through (a Leq-primary fold would short-circuit on equal leading fields).
#[test]
fn derive_ord_record_payload_lexicographic_compare() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
#[derive(Ord)]
enum Pair {
    Pair { x: int, y: int },
}

fn main() {
    let a = Pair::Pair { x: 1, y: 2 };
    let b = Pair::Pair { x: 1, y: 3 };
    let c = Pair::Pair { x: 2, y: 0 };
    write(stdout(), to_bytes(format("%z,", a < b)));
    write(stdout(), to_bytes(format("%z,", a < c)));
    write(stdout(), to_bytes(format("%z,", b < a)));
    write(stdout(), to_bytes(format("%z,", a <= a)));
    write(stdout(), to_bytes(format("%z", a < a)));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "true,true,false,true,false");
}

#[test]
fn example_attr_ffi_strlen_is_compile_error_for_libc() {
    let mut pipeline = test_pipeline();
    pipeline.grant_dload_stem("c");
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate must have a parent (workspace root)");
    let full = workspace_root.join("examples/attr_ffi.hy");
    let result = pipeline.compile_src_from_file(full.to_str().unwrap());
    assert!(result.is_err());
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::HostDloadDenied))
    );
}

#[test]
fn example_spread_prints_3_and_60() {
    let output = run_example("examples/spread.hy");
    assert_eq!(output, "360");
}

#[test]
fn example_attr_decorator_forwards_args_and_stacks_attrs() {
    let output = run_example("examples/attr_decorator.hy");
    assert_eq!(output, "enterdo_thinghi42");
}

#[test]
fn example_attr_class_decorates_constructor() {
    let output = run_example("examples/attr_class.hy");
    assert_eq!(output, "Point ctor512");
}

#[test]
fn attr_method_forwards_self() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
attr log<T>(fn(...args) -> T target, string message, ...args) -> T {
    write(stdout(), to_bytes(format("%s", message)));
    return target(...args);
}

class Counter {
    pub n: int,
}

impl Counter {
    #[log(message = "bump")]
    pub fn bump() -> int {
        return self.n;
    }
}

fn main() {
    let c = new Counter(7);
    write(stdout(), to_bytes(format("%i", c.bump())));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "bump7");
}

#[test]
fn attr_test_fn_discovered_by_harness() {
    let mut pipeline = test_pipeline();
    pipeline.set_include_tests(true);
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tests/positive/attr_test.hy"),
    )
    .expect("read attr_test.hy");
    let (bytecode, constants) = pipeline.compile_src(&src).expect("compile attr_test.hy");
    let cases = pipeline.test_cases().to_vec();
    assert_eq!(
        cases.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["addition works", "multiply_works"]
    );

    for (_name, offset) in &cases {
        let mut machine = Machine::<128>::default();
        pipeline.wire_host_natives(&mut machine);
        machine.load_program(&bytecode, &constants, pipeline.strings());
        let ret = machine.call_function(*offset, &[]);
        assert!(
            !machine.panicked() && machine.result_is_ok(ret),
            "test case should pass"
        );
    }
}

#[test]
fn example_perf_tak_prints_expected() {
    let output = run_example("examples/perf/tak.hy");
    assert_eq!(output, "7");
}

#[test]
fn example_perf_fib_prints_checksum() {
    // Prefer `COIL_AUTO_PAR=0` when timing; checksum is identical either way.
    let output = run_example("examples/perf/fib.hy");
    assert_eq!(output, "2178309");
}

#[test]
fn example_perf_numeric_prints_expected_sum() {
    let output = run_example("examples/perf/numeric.hy");
    assert_eq!(output, "1999000");
}

#[test]
fn example_perf_mandelbrot_prints_checksum() {
    let output = run_example("examples/perf/mandelbrot.hy");
    assert_eq!(output, "625885");
}

#[test]
fn example_perf_binary_trees_prints_checksum() {
    let output = run_example("examples/perf/binary_trees.hy");
    assert_eq!(output, "135854");
}

#[test]
fn example_perf_array_mut_prints_expected() {
    let output = run_example("examples/perf/array_mut.hy");
    assert_eq!(output, "2000");
}

#[test]
fn example_perf_dict_hot_prints_expected() {
    let output = run_example("examples/perf/dict_hot.hy");
    assert_eq!(output, "6000");
}

#[test]
fn example_perf_operators_loop_prints_expected() {
    let output = run_example("examples/perf/operators_loop.hy");
    assert_eq!(output, "149912");
}

#[test]
fn example_perf_coro_ping_prints_expected() {
    let output = run_example("examples/perf/coro_ping.hy");
    assert_eq!(output, "124750");
}

#[test]
fn example_io_bytes_prints_25532() {
    let output = run_example("examples/io_bytes.hy");
    assert_eq!(output, "25532");
}

#[test]
fn example_io_file_prints_2() {
    let output = run_example("examples/io_file.hy");
    assert_eq!(output, "2");
}

#[test]
fn example_io_eof_prints_eof() {
    let output = run_example("examples/io_eof.hy");
    assert_eq!(output, "eof");
}

#[test]
fn example_io_text_prints_hello2() {
    let output = run_example("examples/io_text.hy");
    assert_eq!(output, "hello2");
}

#[test]
fn example_io_udp_prints_2() {
    let output = run_example("examples/io_udp.hy");
    assert_eq!(output, "2");
}

#[test]
fn io_tcp_helper_hostinvokes_are_wired() {
    let output = run_example_src(
        r#"
use io::{open, stdout, write};
use io::net::tcp::{accept, connect_timeout, listen, local_addr, set_nodelay, shutdown};
use string::{format, to_bytes};

fn main() {
    let path = "coil_io_timeout_tcp_helpers.bin";
    let file = open(path, "w")?;

    let local_on_file = match local_addr(file) {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };
    let nodelay_on_file = match set_nodelay(file, true) {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };
    let shutdown_on_file = match shutdown(file, 2) {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };

    let listener = listen("127.0.0.1", 0)?;
    let addr = local_addr(listener)?;
    let accept_code = match accept(listener) {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };
    let connect_code = match connect_timeout("127.0.0.1", 1, 1) {
        Result::Ok(_) => 2,
        Result::Err(_) => 2,
    };

    write(stdout(), to_bytes(format("%i%i%i%i%i", local_on_file, nodelay_on_file, shutdown_on_file, accept_code, connect_code)));
}
"#,
    );
    assert_eq!(output, "11112");
}

/// Nested IO HostInvoke (`read_to_end(open(...)?)`) must leave the stream on
/// the stack as the MakeTuple element, not the outer native id.
#[test]
fn example_io_nested_host_prints_3() {
    let output = run_example("examples/io_nested_host.hy");
    assert_eq!(output, "3");
}

/// Nested IO as the first of two HostInvoke args (`write(open(...), buf)`).
/// Outer arity > 1 — MakeTuple must pack the stream, not the outer native id.
#[test]
fn example_io_nested_write_prints_2() {
    let output = run_example("examples/io_nested_write.hy");
    assert_eq!(output, "2");
}

/// Prelude `block_on` drives a coroutine to its completion value.
#[test]
fn example_block_on_io_prints_2() {
    let output = run_example("examples/block_on_io.hy");
    assert_eq!(output, "2");
}

/// Top-level `drive` with no waiters returns 0 (smoke for host wiring).
#[test]
fn example_io_await_drive_prints_0() {
    let output = run_example("examples/io_await.hy");
    assert_eq!(output, "0");
}

/// Cooperative `await_*` inside coroutines + `wait_ready` batch poll.
#[test]
fn example_io_wait_ready_prints_ok() {
    let output = run_example("examples/io_wait_ready.hy");
    assert_eq!(output, "ok");
}

/// Nested `let server = match accept_wait(listener) { … }` inside a loop must handle
/// multiple connections (binding matches omit the fusion barrier).
#[test]
fn while_accept_match_write_handles_two_connections() {
    use std::io::Read;
    use std::net::TcpStream;
    use std::thread;
    use std::time::Duration;

    const PORT: i64 = 41_299;
    let src = r#"
use io::{close, stdout, write};
use io::sync::{accept_wait};
use io::net::tcp::{listen};
use string::{to_bytes};

fn main() {
    let listener = match listen("127.0.0.1", 41299) {
        Result::Ok(s) => s,
        Result::Err(_) => panic "listen",
    };
    let msg = to_bytes("ok");
    let count = 0;
    while count < 2 {
        let server = match accept_wait(listener) {
            Result::Ok(s) => s,
            Result::Err(_) => panic "accept",
        };
        match write(server, msg) {
            Result::Ok(_) => 0,
            Result::Err(_) => panic "write",
        };
        match close(server) {
            Result::Ok(_) => 0,
            Result::Err(_) => 0,
        };
        count = count + 1;
    }
    write(stdout(), to_bytes("ok"));
}
"#;

    let server = thread::spawn(|| run_example_src(src));

    for round in 0..2 {
        let mut connected = false;
        for attempt in 0..80 {
            match TcpStream::connect(("127.0.0.1", PORT as u16)) {
                Ok(mut s) => {
                    let mut buf = [0u8; 2];
                    s.read_exact(&mut buf).expect("read");
                    assert_eq!(&buf, b"ok");
                    connected = true;
                    break;
                }
                Err(_) if attempt + 1 < 80 => thread::sleep(Duration::from_millis(25)),
                Err(e) => panic!("connect round {round}: {e}"),
            }
        }
        assert!(connected, "server never accepted round {round}");
        thread::sleep(Duration::from_millis(20));
    }

    let output = server.join().expect("server thread");
    assert_eq!(output, "ok");
}

/// `let x = match …` inside `while` must not corrupt loop locals across iterations.
#[test]
fn while_let_result_ok_panic_match_preserves_locals() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let i = 0;
    let acc = 0;
    while i < 3 {
        let v = match Result::Ok(i) {
            Result::Ok(x) => x,
            Result::Err(_) => panic "tick",
        };
        acc = acc + v;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i,%i", acc, i)));
}
"#,
    );
    assert_eq!(output, "3,3");
}

/// Same guard for `for-in` loop bodies (not only `while`).
#[test]
fn for_in_let_match_preserves_accumulator() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let items = [1, 2, 3];
    let acc = 0;
    for n in items {
        let v = match Result::Ok(n) {
            Result::Ok(x) => x + 1,
            Result::Err(_) => panic "tick",
        };
        acc = acc + v;
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
"#,
    );
    assert_eq!(output, "9");
}

/// Err arm in a binding match must still panic with the literal message.
#[test]
fn let_result_ok_panic_match_err_path_panics() {
    let mut pipeline = test_pipeline();
    let (bytecode, constants) = pipeline
        .compile_src(
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
    let shared = SharedBuf::new();
    let mut machine = Machine::<128>::default();
    machine.with_output(shared.clone());
    machine.run_raw(
        &bytecode,
        &constants,
        pipeline.strings(),
        pipeline.static_slot_count(),
    );
    assert!(machine.panicked(), "expected language-level panic on Err");
    let _ = machine.restore_output();
    let s = shared.into_utf8();
    assert_eq!(s, "panic: accept");
}

/// `x = match …` reassignment inside a loop uses the same binding-context
/// match lowering as `let x = match …`.
#[test]
fn while_assignment_match_preserves_locals() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let i = 0;
    let v = 0;
    while i < 3 {
        v = match Result::Ok(i) {
            Result::Ok(x) => x,
            Result::Err(_) => panic "tick",
        };
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", v)));
}
"#,
    );
    assert_eq!(output, "2");
}

/// Standalone virtual `main` for a green `test("…")` suite exits cleanly.
#[test]
fn harness_virtual_main_passes_when_all_asserts_ok() {
    let output = run_harness_src(
        r#"
test("ok") {
    assert(true)?;
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
    let (bytecode, constants) = pipeline
        .compile_src(
            r#"
test("broken") {
    assert(false)?;
}
"#,
        )
        .expect("compile");
    let shared = SharedBuf::new();
    let mut machine = Machine::<128>::default();
    machine.with_output(shared.clone());
    pipeline.wire_host_natives(&mut machine);
    machine.set_program_debug(pipeline.program_debug());
    machine.run_raw(
        &bytecode,
        &constants,
        pipeline.strings(),
        pipeline.static_slot_count(),
    );
    let _ = machine.restore_output();
    assert!(
        machine.panicked(),
        "virtual main must panic when a case fails"
    );
    let output = shared.into_utf8();
    assert!(
        output.contains("> Test \"broken\" failed"),
        "expected failure banner, got {output:?}"
    );
}

/// CLI-style isolation: each case is `call_function`'d independently so a
/// soft failure does not prevent later cases from running.
#[test]
fn harness_isolated_call_function_continues_after_soft_fail() {
    let mut pipeline = test_pipeline();
    pipeline.set_include_tests(true);
    let (bytecode, constants) = pipeline
        .compile_src(
            r#"
test("a") { assert(true)?; }
test("b") { assert(false)?; }
test("c") { assert(1 + 1 == 2)?; }
"#,
        )
        .expect("compile");
    let cases = pipeline.test_cases().to_vec();
    assert_eq!(cases.len(), 3);
    assert_eq!(
        cases.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["a", "b", "c"]
    );

    let mut results = Vec::new();
    for (name, offset) in &cases {
        let mut machine = Machine::<128>::default();
        pipeline.wire_host_natives(&mut machine);
        machine.load_program(&bytecode, &constants, pipeline.strings());
        let ret = machine.call_function(*offset, &[]);
        let ok = !machine.panicked() && machine.result_is_ok(ret);
        results.push((name.as_str(), ok));
    }
    assert_eq!(results, [("a", true), ("b", false), ("c", true)]);
}

/// Hard-`panic` path: each case is still isolated (fresh VM + unwind fence),
/// matching `run_test_case` in the CLI so a VM abort does not fail-fast.
#[test]
fn harness_isolated_call_function_continues_after_hard_panic() {
    let mut pipeline = test_pipeline();
    pipeline.set_include_tests(true);
    let (bytecode, constants) = pipeline
        .compile_src(
            r#"
test("boom") { panic "x"; }
test("after") { assert(true)?; }
"#,
        )
        .expect("compile");
    let cases = pipeline.test_cases().to_vec();
    assert_eq!(
        cases.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["boom", "after"]
    );

    let mut results = Vec::new();
    for (name, offset) in &cases {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut machine = Machine::<128>::default();
            pipeline.wire_host_natives(&mut machine);
            machine.load_program(&bytecode, &constants, pipeline.strings());
            let ret = machine.call_function(*offset, &[]);
            !machine.panicked() && machine.result_is_ok(ret)
        }));
        let ok = match outcome {
            Ok(ok) => ok,
            Err(_) => false,
        };
        results.push((name.as_str(), ok));
    }
    assert_eq!(results, [("boom", false), ("after", true)]);
}

/// Match arms that reuse a binding name with different payload types
/// must resolve field access against *that arm's* type. A flat
/// `codegen_var_types` side-table last-wins would make `p.y` emit
/// `LoadField(0)` (against Rect) and return `x` instead of `y`.
#[test]
fn match_arm_reused_binding_name_field_access_uses_arm_type() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Point {
    Point { x: int, y: int },
}

enum Rect {
    Rect { w: int, h: int },
}

enum Shape {
    Pt(Point),
    Rc(Rect),
}

fn get(Shape s) -> int {
    return match s {
        Shape::Pt(p) => p.y,
        Shape::Rc(p) => p.h,
    };
}

fn main() {
    write(stdout(), to_bytes(format("%i", get(Shape::Pt(Point::Point { x: 1, y: 2 })))));
    write(stdout(), to_bytes(format("%i", get(Shape::Rc(Rect::Rect { w: 3, h: 4 })))));
}
"#,
    );
    assert_eq!(output, "24");
}

/// Polymorphic payloads (`Option<Point>`, `Box<T>`) must not push
/// registry schema placeholders (`Con("T")`) onto the per-arm override
/// stack — that shadows the instantiated side-table type and makes
/// `p.y` emit `LoadField(0)` (returns `x` / `1` instead of `y` / `2`).
#[test]
fn match_poly_payload_field_access_uses_instantiated_type() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Point {
    Point { x: int, y: int },
}

enum Box<T> {
    Full(T),
}

fn from_option(Option<Point> o) -> int {
    return match o {
        Option::None => 0,
        Option::Some(p) => p.y,
    };
}

fn from_box(Box<Point> b) -> int {
    return match b {
        Box::Full(p) => p.y,
    };
}

fn main() {
    write(stdout(), to_bytes(format("%i", from_option(Option::Some(Point::Point { x: 1, y: 2 })))));
    write(stdout(), to_bytes(format("%i", from_box(Box::Full(Point::Point { x: 1, y: 2 })))));
}
"#,
    );
    // Broken override → "11"; correct → "22".
    assert_eq!(output, "22");
}

/// P0: early-loop flag reassignment sticks while later locals stay live.
#[test]
fn store_pop_early_flag_sticks_with_later_locals() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let got = 0;
    let a = 1;
    let b = 2;
    let c = 3;
    let i = 0;
    while i < 3 {
        got = 1;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", got)));
    write(stdout(), to_bytes(format("%i", a + b + c)));
}
"#,
    );
    assert_eq!(output, "16");
}

/// P1: empty Vec + push + index round-trip (and under GC pressure).
#[test]
fn empty_array_append_and_index_round_trip() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let arr: Vec<int> = Vec::new();
    arr.push(4);
    arr.push(1);
    arr.push(4);
    write(stdout(), to_bytes(format("%i", len(arr))));
    write(stdout(), to_bytes(format("%i", arr[0])));
    write(stdout(), to_bytes(format("%i", arr[2])));
    let i = 0;
    while i < 80 {
        arr.push(i);
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", arr[0])));
}
"#,
    );
    // len=3, arr[0]=4, arr[2]=4, arr[0]=4 after growth
    assert_eq!(output, "3444");
}

/// P1: `arr[i] = x` then read-back.
#[test]
fn array_index_store_round_trip() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let arr = [0, 0, 0];
    arr[1] = 42;
    write(stdout(), to_bytes(format("%i", arr[0])));
    write(stdout(), to_bytes(format("%i", arr[1])));
    write(stdout(), to_bytes(format("%i", arr[2])));
}
"#,
    );
    assert_eq!(output, "0420");
}

/// P3: `return -1;` compiles and runs.
#[test]
fn return_negative_one_works() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn neg() -> int { return -1; }
fn main() {
    write(stdout(), to_bytes(format("%i", neg())));
    write(stdout(), to_bytes(format("%i", 0 - 1)));
}
"#,
    );
    assert_eq!(output, "-1-1");
}

/// P4: natural Ok/Ok/Err arm order (Err last) must not panic at codegen.
#[test]
fn nested_match_ok_arms_before_err_dispatches() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn unwrap_result(Result r) -> int {
    return match r {
        Result::Ok(Option::Some(v)) => v,
        Result::Ok(Option::None) => 0,
        Result::Err(_) => -1,
    };
}
fn main() {
    write(stdout(), to_bytes(format("%i", unwrap_result(Result::Ok(Option::Some(42))))));
    write(stdout(), to_bytes(format("%i", unwrap_result(Result::Ok(Option::None)))));
    write(stdout(), to_bytes(format("%i", unwrap_result(Result::Err("oops")))));
}
"#,
    );
    assert_eq!(output, "420-1");
}

/// P6: class field holding an enum round-trips via GetField.
#[test]
fn class_enum_field_access_round_trip() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Status {
    Ready,
    Done(int),
}

class Box {
    pub status: Status,
}

impl Box {
    pub fn get() -> Status {
        return self.status;
    }
}

fn main() {
    let b = new Box(Status::Done(9));
    let s = b.get();
    write(stdout(), to_bytes(format("%i", match s {
        Status::Ready => 0,
        Status::Done(v) => v,
    })));
    write(stdout(), to_bytes(format("%i", match b.status {
        Status::Ready => 0,
        Status::Done(v) => v,
    })));
}
"#,
    );
    assert_eq!(output, "99");
}

/// P6: match-bound constructor payloads must land in `codegen_var_types`
/// so field access uses the payload enum's LoadField index — not the
/// defensive `LoadField(0)` fallback (which silently returns the wrong
/// field when the target is not field 0).
#[test]
fn match_bound_enum_field_access_uses_correct_index() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Info {
    Info { kind: int, code: int },
}

enum Wrap {
    Empty,
    Full(Info),
}

fn read_code(Wrap w) -> int {
    return match w {
        Wrap::Empty => 0,
        Wrap::Full(e) => e.code,
    };
}

fn main() {
    let w = Wrap::Full(Info::Info { kind: 1, code: 42 });
    write(stdout(), to_bytes(format("%i", read_code(w))));
    write(stdout(), to_bytes(format("%i", match w {
        Wrap::Empty => 0,
        Wrap::Full(e) => e.kind,
    })));
}
"#,
    );
    // Pre-fix: e.code → LoadField(0) → kind (1), not code (42).
    assert_eq!(output, "421");
}

#[test]
fn example_overload_prints_15() {
    let output = run_example("examples/overload.hy");
    assert_eq!(output, "15");
}

#[test]
fn example_type_overload_prints_typed_tags() {
    let output = run_example("examples/type_overload.hy");
    assert_eq!(output, "i:7f:1.5s:hi");
}

#[test]
fn example_fn_value_prints_423() {
    let output = run_example("examples/fn_value.hy");
    assert_eq!(output, "423");
}

#[test]
fn example_lambda_prints_42() {
    let output = run_example("examples/lambda.hy");
    assert_eq!(output, "42");
}

/// Disk-module imports (e.g. `io::sync::write_all`) are file-level globals.
/// After `take_and_isolate` they must be rebound like virtual imports — not
/// treated as missing captures inside lambdas.
#[test]
fn disk_import_usable_inside_lambda_without_capture() {
    let output = run_example_src(
        r#"
use io::{stdout};
use io::sync::{write_all};
use string::{to_bytes};
fn main() {
    let f = fn () => write_all(stdout(), to_bytes("ok"));
    f()?;
}
"#,
    );
    assert_eq!(output, "ok");
}

/// Aliased disk imports must rebind under the local alias name.
#[test]
fn disk_import_alias_usable_inside_lambda() {
    let output = run_example_src(
        r#"
use io::{stdout};
use io::sync::{write_all as wa};
use string::{to_bytes};
fn main() {
    let f = fn () => wa(stdout(), to_bytes("ok"));
    f()?;
}
"#,
    );
    assert_eq!(output, "ok");
}

/// Same rebind rule for `defer` bodies that call disk-module imports.
#[test]
fn disk_import_usable_inside_defer_without_capture() {
    let output = run_example_src(
        r#"
use io::{stdout};
use io::sync::{write_all};
use string::{to_bytes};
fn main() {
    defer { write_all(stdout(), to_bytes("d")); }
    write_all(stdout(), to_bytes("a"));
}
"#,
    );
    assert_eq!(output, "ad");
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

/// Nested typed lambdas must keep Fragment/Argument NodeIds lockstep with
/// codegen (`assign_fn_arg_node_ids`); a desync surfaces as wrong results or
/// compile failure rather than Identifier span prefer.
#[test]
fn nested_typed_lambdas_print_sum() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let add = fn (int x) => fn (int y) use (x) => x + y;
    let add40 = add(40);
    write(stdout(), to_bytes(format("%i", add40(2))));
}
"#,
    );
    assert_eq!(output, "42");
}

#[test]
fn example_method_overload_prints_1116() {
    let output = run_example("examples/method_overload.hy");
    assert_eq!(output, "1116");
}

/// Named under-apply must build a partial whose holes fill in declaration
/// order at CallIndirect time (not print the named value as the only arg).
#[test]
fn named_partial_application_completes_and_prints_3() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn add(int a, int b) -> int { return a + b; }
fn main() {
    let g = add(a: 1);
    write(stdout(), to_bytes(format("%i", g(2))));
}
"#,
    );
    assert_eq!(output, "3");
}

/// Nested partials (`f(1)` → `p(2)` → `q(3)`) must merge filled masks without
/// clobbering earlier holes — a common ObjFn regression.
#[test]
fn nested_partial_application_prints_6() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn add3(int a, int b, int c) -> int { return a + b + c; }
fn main() {
    let p = add3(1);
    let q = p(2);
    write(stdout(), to_bytes(format("%i", q(3))));
}
"#,
    );
    assert_eq!(output, "6");
}

/// Fixed-N vs rest-K (N < K) must dispatch by argc: exact fixed wins at N;
/// rest handles K and above. Typechecker unit tests alone miss wrong CALL targets.
#[test]
fn fixed_vs_rest_overload_dispatches_at_runtime() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn f(int x) -> int { return x * 10; }
fn f(int x, int y, int... xs) -> int { return x + y + len(xs); }
fn main() {
    write(stdout(), to_bytes(format("%i", f(1))));
    write(stdout(), to_bytes(format("%i", f(1, 2))));
    write(stdout(), to_bytes(format("%i", f(1, 2, 3, 4))));
}
"#,
    );
    assert_eq!(output, "1035");
}

// ── Harness stripping + cross-feature integration ─────────────────────────────

#[test]
fn production_compile_strips_harness_declarations() {
    let mut pipeline = test_pipeline();
    let (bytecode, constants) = pipeline
        .compile_src(
            r#"
use io::{stdout, write};
use string::{format, to_bytes};
test("hidden") { assert(false)?; }
test("also hidden") { assert(false)?; }
fn main() { write(stdout(), to_bytes("ok")); }
"#,
        )
        .expect("compile");
    assert!(
        pipeline.test_cases().is_empty(),
        "production compile must not register harness cases"
    );
    let output = run_bytecode(bytecode, constants, &pipeline, None);
    assert_eq!(output, "ok");
}

#[test]
fn include_tests_flag_embeds_harness_metadata() {
    let (pipeline, _bc, _constants) = compile_src_with_tests(
        r#"
test("via attr") { assert(true)?; }
test("via block") { assert(true)?; }
"#,
    );
    assert_eq!(pipeline.test_cases().len(), 2);
    assert_eq!(pipeline.test_cases()[0].0, "via attr");
    assert_eq!(pipeline.test_cases()[1].0, "via block");
}

#[test]
fn attr_decorator_with_overloaded_functions_forwards_each_arity() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
attr log<T>(fn(...args) -> T target, string message, ...args) -> T {
    write(stdout(), to_bytes(format("%s", message)));
    return target(...args);
}

#[log(message = "nullary")]
fn do_thing() -> int { return 0; }

#[log(message = "unary")]
fn do_thing(int x) -> int { return x; }

fn main() {
    write(stdout(), to_bytes(format("%i", do_thing())));
    write(stdout(), to_bytes(format("%i", do_thing(42))));
}
"#,
    );
    assert_eq!(output, "nullary0unary42");
}

#[test]
fn spread_with_partial_application_forwards_remaining_args() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn add3(int a, int b, int c) -> int { return a + b + c; }
fn main() {
    let p = add3(1);
    write(stdout(), to_bytes(format("%i", p(...(2, 3)))));
}
"#,
    );
    assert_eq!(output, "6");
}

#[test]
fn named_call_args_on_overloaded_functions_dispatch_correctly() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn greet(string name) -> string { return name; }
fn greet(string name, int age) -> string { return format("%s:%i", name, age); }
fn main() {
    write(stdout(), to_bytes(format("%s", greet(name: "Ada"))));
    write(stdout(), to_bytes(format("%s", greet(name: "Grace", age: 40))));
}
"#,
    );
    assert_eq!(output, "AdaGrace:40");
}

#[test]
fn attr_on_async_fn_rejected_at_compile_time() {
    // Attr-body-crosses-yield desugaring for a coroutine target used to
    // recurse deep enough (~1.5-2 MiB) to risk the default per-test thread
    // stack before infer_inner/do_compile were split up; reuse the example
    // pool's 8 MiB workers instead of spawn/join (COI-88).
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
attr log<T>(fn(...args) -> T target, string message, ...args) -> T {
    yield 99;
    return target(...args);
}
#[log(message = "coro")]
async fn counter() {
    yield 1;
}
fn main() {
    let h = counter();
    write(stdout(), to_bytes(format("%i", resume h)));
}
"#
    .to_string();
    let is_err = Arc::new(Mutex::new(false));
    let flag = Arc::clone(&is_err);
    run_on_example_stack("attr-async-diag".into(), move || {
        *flag.lock().unwrap_or_else(|e| e.into_inner()) =
            test_pipeline().compile_src(&src).is_err();
        String::new()
    });
    assert!(
        *is_err.lock().unwrap_or_else(|e| e.into_inner()),
        "attrs that yield outside target(...args) must be rejected on async fn"
    );
}

#[test]
fn rest_overload_with_attr_logging_forwards_pack() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
attr log<T>(fn(...args) -> T target, string message, ...args) -> T {
    write(stdout(), to_bytes(format("%s", message)));
    return target(...args);
}

#[log(message = "sum")]
fn total(int... xs) -> int {
    return len(xs);
}

fn main() {
    write(stdout(), to_bytes(format("%i", total(1, 2, 3))));
}
"#,
    );
    assert_eq!(output, "sum3");
}

/// Prefix args + spread pack must flatten in source order. A buggy flatten that
/// drops the prefix or reverses pack elements would still typecheck for
/// `add3(int,int,int)` but print the wrong sum.
#[test]
fn spread_mixed_prefix_and_pack_prints_6() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn add3(int a, int b, int c) -> int { return a + b + c; }
fn main() {
    write(stdout(), to_bytes(format("%i", add3(1, ...(2, 3)))));
}
"#,
    );
    assert_eq!(output, "6");
}

/// Let-bound packs exercise the typed-lookup codegen path (not the literal
/// Tuple/Array shortcuts). Wrong Index expansion would silently mis-bind.
#[test]
fn spread_let_bound_tuple_prints_6() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn add3(int a, int b, int c) -> int { return a + b + c; }
fn main() {
    let pack = (1, 2, 3);
    write(stdout(), to_bytes(format("%i", add3(...pack))));
}
"#,
    );
    assert_eq!(output, "6");
}

/// Positional attr extras (`#[log("enter")]`) must bind to the first extra
/// parameter the same way named extras do.
#[test]
fn attr_positional_extra_forwards_literal() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
attr log<T>(fn(...args) -> T target, string message, ...args) -> T {
    write(stdout(), to_bytes(format("%s", message)));
    return target(...args);
}

#[log("enter")]
fn do_thing(int x) -> int { return x; }

fn main() {
    write(stdout(), to_bytes(format("%i", do_thing(7))));
}
"#,
    );
    assert_eq!(output, "enter7");
}

/// Stacking is Python-style: first listed attr is outermost. Reversing the
/// expand loop would swap the print order without failing typecheck.
#[test]
fn stacked_attrs_apply_outer_first() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
attr outer<T>(fn(...args) -> T target, ...args) -> T {
    write(stdout(), to_bytes("O"));
    return target(...args);
}
attr inner<T>(fn(...args) -> T target, ...args) -> T {
    write(stdout(), to_bytes("I"));
    return target(...args);
}

#[outer]
#[inner]
fn f() -> int { return 1; }

fn main() {
    write(stdout(), to_bytes(format("%i", f())));
}
"#,
    );
    assert_eq!(output, "OI1");
}

#[test]
fn attr_inlining_rewrites_target_in_all_expression_contexts() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
attr wrap_if<T>(fn(...args) -> T target, ...args) -> T {
    if true {
        return target(...args);
    }
    return 0;
}

attr wrap_for<T>(fn(...args) -> T target, ...args) -> T {
    let i = 0;
    while i < 1 {
        return target(...args);
    }
    return 0;
}

attr wrap_while<T>(fn(...args) -> T target, ...args) -> T {
    while (true) {
        return target(...args);
    }
    return 0;
}

attr wrap_print<T>(fn(...args) -> T target, ...args) -> T {
    write(stdout(), to_bytes(format("%s", "x")));
    return target(...args);
}

#[wrap_if]
fn a() -> int { return 10; }

#[wrap_for]
fn b() -> int { return 20; }

#[wrap_while]
fn c() -> int { return 30; }

#[wrap_print]
fn d() -> int { return 40; }

fn main() {
    write(stdout(), to_bytes(format("%i", a())));
    write(stdout(), to_bytes(format("%i", b())));
    write(stdout(), to_bytes(format("%i", c())));
    write(stdout(), to_bytes(format("%i", d())));
}
"#,
    );
    assert_eq!(output, "102030x40");
}

/// End-to-end TailCall: self-recursive accumulator must print the correct sum.
#[test]
fn tail_recursive_sum_to_prints_15() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sum_to(int n, int acc) -> int {
    if n <= 0 { return acc; }
    return sum_to(n - 1, acc + n);
}
fn main() {
    write(stdout(), to_bytes(format("%i", sum_to(5, 0))));
}
"#,
    );
    assert_eq!(output, "15");
}

/// Const-fold `if` with strict `<` equality boundary takes else (Le vs Leq).
#[test]
fn const_if_strict_lt_equality_prints_else() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    if 5 < 5 {
        write(stdout(), to_bytes(format("%i", 1)));
    } else {
        write(stdout(), to_bytes(format("%i", 0)));
    }
}
"#,
    );
    assert_eq!(output, "0");
}

/// `while false` must skip the body entirely.
#[test]
fn const_while_false_skips_body() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    while false {
        write(stdout(), to_bytes(format("%i", 1)));
    }
    write(stdout(), to_bytes(format("%i", 2)));
}
"#,
    );
    assert_eq!(output, "2");
}

/// Constant-trip `for` unroll must advance `i` via `step` between bodies.
/// If unroll engaged but skipped `step`, `s = s + i` would stay `0`.
#[test]
fn const_for_unroll_prints_sum() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let s = 0;
    let i = 0;
    while i < 4 {
        s = s + i;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", s)));
}
"#,
    );
    assert_eq!(output, "6");
}

/// IL unroll of counted `while` must still run the induction step each trip.
#[test]
fn const_while_unroll_prints_sum() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let s = 0;
    let i = 0;
    while i < 4 {
        s = s + i;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", s)));
}
"#,
    );
    assert_eq!(output, "6");
}

/// Signed `/` is toward zero; arithmetic `>>` would differ for negatives.
#[test]
fn signed_div_toward_zero_prints() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let x = 0 - 5;
    write(stdout(), to_bytes(format("%i", x / 2)));
}
"#,
    );
    assert_eq!(output, "-2");
}

/// Bit identities: `x | 0` and `x ^ x` keep value / yield zero (COI-123).
#[test]
fn bitop_identities_print() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let x = 7;
    write(stdout(), to_bytes(format("%i %i", x | 0, x ^ x)));
}
"#,
    );
    assert_eq!(output, "7 0");
}

/// Invariant `t = 42` in a counted `while` must still be visible after the loop
/// (COI-120 sinks the store rather than dropping a live slot).
#[test]
fn loop_invariant_store_live_after_prints() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let t = 0;
    let i = 0;
    while i < 10 {
        t = 42;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", t)));
}
"#,
    );
    assert_eq!(output, "42");
}

/// Loop-variant stores must keep the last iteration's value.
#[test]
fn loop_variant_store_live_after_prints() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let t = 0;
    let i = 0;
    while i < 10 {
        t = i;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", t)));
}
"#,
    );
    assert_eq!(output, "9");
}

/// Same induction check with `i <= 2` (3 trips: 0+1+2).
#[test]
fn const_for_unroll_leq_advances_induction() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let s = 0;
    let i = 0;
    while i <= 2 {
        s = s + i;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", s)));
}
"#,
    );
    assert_eq!(output, "3");
}

/// Range for-in unroll (`0..3`) binds successive values correctly.
#[test]
fn const_range_for_in_unroll_prints() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let s = 0;
    for x in 0..3 {
        s = s + x;
    }
    write(stdout(), to_bytes(format("%i", s)));
}
"#,
    );
    assert_eq!(output, "3");
}

/// `break` disables unroll — first iteration still runs, rest skipped.
#[test]
fn for_with_break_still_stops_early() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let s = 0;
    let i = 0;
    while i < 5 {
        s = s + i;
        break;
    }
    write(stdout(), to_bytes(format("%i", s)));
}
"#,
    );
    assert_eq!(output, "0");
}

/// Fused `if i == k { continue/break }` inverts to `*Jmpt` (COI-87). Sum is
/// 0+1+2+4+5+6 = 18 (skip 3, stop before 7).
#[test]
fn for_continue_and_break_guards_invert_to_jmpt() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let sum = 0;
    let i = 0;
    while i < 10 {
        if i == 3 { i = i + 1; continue; }
        if i == 7 { break; }
        sum = sum + i;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", sum)));
}
"#,
    );
    assert_eq!(output, "18");
}

/// `if !done { break }` must take LogNotJmpt polarity (COI-87).
#[test]
fn not_flag_break_log_not_jmpt_runs() {
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
    let mut pipeline = test_pipeline();
    let (bytecode, _) = pipeline.compile_src(src).expect("compile");
    assert!(
        bytecode
            .iter()
            .any(|b| matches!(b.bytecode(), common::Instruction::LogNotJmpt)),
        "expected LogNotJmpt in bytecode"
    );
    assert_eq!(run_example_src(src), "0");
}

/// Two-local `if a < b { break }` fuses to BinSlotSlotJmpt and takes the break.
#[test]
fn two_local_compare_break_bin_slot_slot_jmpt_runs() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let a = 1;
    let b = 2;
    let n = 0;
    while (n < 10) {
        if a < b { break; }
        n = n + 1;
    }
    write(stdout(), to_bytes(format("%i", n)));
}
"#;
    let mut pipeline = test_pipeline();
    let (bytecode, _) = pipeline.compile_src(src).expect("compile");
    assert!(
        bytecode
            .iter()
            .any(|b| matches!(b.bytecode(), common::Instruction::BinSlotSlotJmpt)),
        "expected BinSlotSlotJmpt in bytecode"
    );
    assert_eq!(run_example_src(src), "0");
}

/// Tiny direct-call inlining must preserve call semantics end-to-end.
#[test]
fn tiny_add_inlined_prints_7() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn add(int a, int b) -> int { return a + b; }
fn main() {
    write(stdout(), to_bytes(format("%i", add(3, 4))));
}
"#,
    );
    assert_eq!(output, "7");
}

/// `x * 2^n` / `2^n * x` / `x * const(2^n)` must lower to SHL but still
/// compute the same values as MUL (wrong shift amount is silent otherwise).
#[test]
fn mul_strength_reduce_to_shl_prints_correct_values() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn scale_rhs(int x) -> int { return x * 8; }
fn scale_lhs(int x) -> int { return 4 * x; }
fn scale_const(int x) -> int {
    const K = 16;
    return x * K;
}
fn scale_one(int x) -> int { return x * 1; }
fn scale_six(int x) -> int { return x * 6; }
fn scale_negative_x(int x) -> int { return x * 8; }
fn main() {
    write(stdout(), to_bytes(format("%i,", scale_rhs(5))));
    write(stdout(), to_bytes(format("%i,", scale_lhs(7))));
    write(stdout(), to_bytes(format("%i,", scale_const(3))));
    write(stdout(), to_bytes(format("%i,", scale_one(9))));
    write(stdout(), to_bytes(format("%i,", scale_six(7))));
    write(stdout(), to_bytes(format("%i", scale_negative_x(0 - 3))));
}
"#,
    );
    assert_eq!(output, "40,28,48,9,42,-24");
}

/// Early-return diamond callees are tiny-inlined; both arms must still evaluate
/// correctly at the call site.
#[test]
fn early_return_callee_both_arms_correct() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn early(int n, int is_neg) -> int {
    if is_neg == 1 {
        return 0 - 1;
    }
    return n * 2;
}
fn main() {
    write(stdout(), to_bytes(format("%i,", early(4, 1))));
    write(stdout(), to_bytes(format("%i", early(4, 0))));
}
"#,
    );
    assert_eq!(output, "-1,8");
}

/// Pure-arg reorder must keep CALL arg order while evaluating pure temps before
/// effectful args (print from effect runs first; sink still sees a=2, b=10).
#[test]
fn pure_arg_reorder_preserves_order_and_effects() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn effect() -> int {
    write(stdout(), to_bytes(format("%i,", 7)));
    return 2;
}
fn sink(int a, int b) -> int {
    write(stdout(), to_bytes(format("%i,", a + b)));
    return a + b;
}
fn main() {
    write(stdout(), to_bytes(format("%i", sink(effect(), 10))));
}
"#,
    );
    assert_eq!(output, "7,12,12");
}

/// Predicate peel base path and recursive path must both return correct values.
#[test]
fn predicate_peel_base_and_recursive_paths() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn other(int n) -> int {
    return n;
}
fn base(int n) -> int {
    if n <= 0 {
        return 1;
    }
    return other(n) + 1;
}
fn main() {
    write(stdout(), to_bytes(format("%i,", base(0))));
    write(stdout(), to_bytes(format("%i", base(5))));
}
"#,
    );
    assert_eq!(output, "1,6");
}

/// One-level self-unroll must not change recursive fib results.
#[test]
fn self_unroll_fib_still_correct() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    write(stdout(), to_bytes(format("%i", fib(10))));
}
"#,
    );
    assert_eq!(output, "55");
}

/// Auto-par fork-join of pure recursive `fib(n-1)+fib(n-2)` must stay correct.
#[test]
fn auto_par_fib_still_correct() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    write(stdout(), to_bytes(format("%i", fib(12))));
}
"#,
    );
    assert_eq!(output, "144");
}

/// Constant sites above `COIL_PAR_THRESHOLD` must emit `__coil_par_*` and stay correct.
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
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("auto-par fib should compile");
    assert!(
        pipeline.function_offset("__coil_par_fib_22").is_some(),
        "expected static specialization for fib(22)"
    );
    assert!(
        pipeline.function_offset("__coil_par_fib_21").is_some(),
        "expected chain specialization for fib(21)"
    );
    // Default threshold is 20 — exact threshold stays sequential.
    assert!(
        pipeline.function_offset("__coil_par_fib_20").is_none(),
        "fib(20) must not get a parallel specialization"
    );
    let output = run_bytecode(bytecode, constants, &pipeline, None);
    assert_eq!(output, "17711");
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
#[max_depth(64)]
fn build(int n) -> Tree {
    if n <= 1 {
        return Tree::Leaf();
    }
    return Tree::Node(build(n - 1), build(n - 2));
}
#[max_depth(64)]
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
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("auto-par enum-ctor should compile");
    assert!(
        pipeline.function_offset("__coil_par_build_21").is_some(),
        "expected an enum-ctor specialization for build(21)"
    );
    // `build` shares fib's recurrence, so the leaf count is fib(22).
    let output = run_bytecode(bytecode, constants, &pipeline, None);
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
        pipeline.function_offset("__coil_par_rec_22").is_none(),
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
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("auto-par sub binop should compile");
    assert!(
        pipeline.function_offset("__coil_par_diff_22").is_some(),
        "expected a Sub specialization for diff(22)"
    );
    let output = run_bytecode(bytecode, constants, &pipeline, None);
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
#[max_depth(4096)]
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
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("auto-par self-call should compile");
    assert!(
        pipeline.function_offset("__coil_par_tak_21_12_6").is_some(),
        "expected a multi-arg specialization for tak(21, 12, 6)"
    );
    assert!(
        pipeline
            .function_offset("__coil_par_tak_24_22_20")
            .is_none(),
        "a narrow x - y gap must not specialize"
    );
    let output = run_bytecode(bytecode, constants, &pipeline, None);
    assert_eq!(output, "12");
}

/// The fair `tak(18, 12, 6)` benchmark load scores just under the threshold, so
/// it must keep running on the sequential original.
#[test]
fn auto_par_fair_tak_load_stays_sequential() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
#[max_depth(4096)]
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
    let (bytecode, constants) = pipeline.compile_src(src).expect("fair tak should compile");
    assert!(
        pipeline.function_offset("__coil_par_tak_18_12_6").is_none(),
        "fair tak(18, 12, 6) must not get a parallel specialization"
    );
    let output = run_bytecode(bytecode, constants, &pipeline, None);
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
        pipeline.function_offset("__coil_par_rec_22").is_none(),
        "impure recursion must not emit auto-par specializations"
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
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("helper-arm IPA should compile");
    assert!(
        pipeline.function_offset("__coil_par_pair_fib_22").is_some(),
        "expected a specialization for pair_fib(22)"
    );
    // fib(22) + fib(21)
    let output = run_bytecode(bytecode, constants, &pipeline, None);
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
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("trivial helper arms should compile");
    assert!(
        pipeline.function_offset("__coil_par_pair_sq_22").is_none(),
        "two multiplies must not buy a spawn"
    );
    // 22² + 21²
    let output = run_bytecode(bytecode, constants, &pipeline, None);
    assert_eq!(output, "925");
}

/// Fork inside an irrefutable match arm keeps evaluable path guards.
#[test]
fn auto_par_irrefutable_match_arm_emits_spec() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
#[max_depth(64)]
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
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("match-arm IPA should compile");
    assert!(
        pipeline.function_offset("__coil_par_fibm_22").is_some(),
        "irrefutable match arm must still specialize"
    );
    let output = run_bytecode(bytecode, constants, &pipeline, None);
    assert_eq!(output, "17711");
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
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("auto-par loop should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_some(),
        "expected a chunk worker for the counted loop"
    );
    // Sum of i*i for i in 0..100, and the induction variable past its range.
    let output = run_bytecode(bytecode, constants, &pipeline, None);
    assert_eq!(output, "328350,100");
}

/// The reduction operator drives the fold: a `*` chunk pair recombines by `MUL`
/// with `1` seeding every chunk but the first. A non-identity seed would square
/// the accumulator's initial value.
#[test]
fn auto_par_loop_product_uses_multiplicative_identity() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let prod = 3;
    let i = 0;
    while i <= 29 {
        prod = prod * 2;
        i += 1;
    }
    write(stdout(), to_bytes(format("%i", prod)));
}
"#,
    );
    // 3 * 2^30; seeding both chunks with 3 would give 9 * 2^30.
    assert_eq!(output, "3221225472");
}

/// Loop-private temps are re-emitted into the worker's frame.
#[test]
fn auto_par_loop_body_temps_still_correct() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn twice(int i) -> int {
    return i + i;
}
fn main() {
    let acc = 0;
    let i = 0;
    while i < 64 {
        let d = twice(i);
        let w = d + 1;
        acc = acc + w;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
"#,
    );
    // sum(2i + 1) for i in 0..64 = 63*64 + 64.
    assert_eq!(output, "4096");
}

/// The accumulator lives in a nested block of a non-`main` function, so the
/// chunk fold has to resolve its slot through the block-binding overlay.
#[test]
fn auto_par_loop_inside_nested_block_of_function() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn step(int i) -> int {
    return i + 3;
}
fn total(int gate) -> int {
    if gate > 0 {
        let acc = 5;
        let i = 0;
        while i < 50 {
            acc = acc + step(i);
            i = i + 1;
        }
        return acc;
    }
    return 0;
}
fn main() {
    write(stdout(), to_bytes(format("%i", total(1))));
}
"#,
    );
    // 5 + sum(i + 3) for i in 0..50 = 5 + 1225 + 150.
    assert_eq!(output, "1380");
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
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("two auto-par loops should compile");
    for name in ["__coil_par_loop_1", "__coil_par_loop_2"] {
        assert!(
            pipeline.function_offset(name).is_some(),
            "expected a chunk worker named {name}"
        );
    }
    let output = run_bytecode(bytecode, constants, &pipeline, None);
    assert_eq!(output, "8555,780");
}

/// A recursive-pure callee inside a chunk worker nests loop IPA under recursive
/// IPA — the join has to help-steal rather than deadlock on a busy pool.
#[test]
fn auto_par_loop_over_recursive_pure_callee() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let acc = 0;
    let i = 0;
    while i < 25 {
        acc = acc + fib(i);
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
"#,
    );
    // sum(fib(i)) for i in 0..25 = fib(26) - 1.
    assert_eq!(output, "121392");
}

/// Bit operators and nested arithmetic in the reduction operand survive the
/// re-emit into the worker's frame.
#[test]
fn auto_par_loop_bit_arithmetic_operand() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let acc = 0;
    let i = 0;
    while i < 40 {
        acc = acc + ((i << 1) & 7);
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
"#,
    );
    // (2i mod 8) cycles 0,2,4,6 ten times.
    assert_eq!(output, "120");
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
    let (bytecode, constants) = pipeline
        .compile_src(src)
        .expect("short loop should compile");
    assert!(
        pipeline.function_offset("__coil_par_loop_1").is_none(),
        "a 20-trip loop cannot pay for a spawn"
    );
    assert_eq!(run_bytecode(bytecode, constants, &pipeline, None), "2470");
}

/// Nested CALL + `let x = f(); if x == k` must not hang: mem_fwd must not
/// turn StorePop;Load into Dup;Store when the store extends tell past TOS
/// (shared-stack CmpJmpf would eat the local — broke http `parse_url`).
#[test]
fn mem_fwd_post_call_store_compare_does_not_hang() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn find_bytes(Vec<byte> hay, Vec<byte> needle) -> int {
    let hn = len(hay);
    let nn = len(needle);
    if nn == 0 { return 0; }
    if nn > hn { return 999999; }
    let i = 0;
    while i + nn <= hn {
        let ok = 1;
        let j = 0;
        while j < nn {
            if hay[i + j] != needle[j] { ok = 0; }
            j = j + 1;
        }
        if ok == 1 { return i; }
        i = i + 1;
    }
    return 999999;
}
fn bytes_slice(Vec<byte> src, int start, int end) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = start;
    while i < end {
        if i < len(src) { out.push(src[i]); }
        i = i + 1;
    }
    return out;
}
fn parse_url(string s) -> int {
    let b = to_bytes(s);
    let sep = Vec::from([58 as byte, 47 as byte, 47 as byte]);
    let sep_at = find_bytes(b, sep);
    if sep_at == 999999 { return -1; }
    let scheme_b = bytes_slice(b, 0, sep_at);
    let rest_start = sep_at + 3;
    let host_end = len(b);
    let host_b = bytes_slice(b, rest_start, host_end);
    return len(scheme_b) + len(host_b);
}
fn main() {
    write(stdout(), to_bytes(format("%i", parse_url("http://example.com/hi"))));
}
"#,
    );
    assert_eq!(output, "18");
}

/// Fused assign / two-local AND-if / packed three-arg call must evaluate correctly.
#[test]
fn fused_assign_and_packed_call_runtime() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn add3(int a, int b, int c) -> int {
    if a < 0 {
        return 0;
    }
    if b < 0 {
        return 0;
    }
    return a + b + c;
}
fn main() {
    let i = 0;
    i = i + 1;
    i = i + 1;
    let flags = 15;
    let mask = 7;
    flags = flags & mask;
    let a = true;
    let b = false;
    let and_ok = 0;
    if a && b {
        and_ok = 1;
    } else {
        and_ok = 2;
    }
    let x = 1;
    let y = 2;
    let z = 3;
    write(stdout(), to_bytes(format("%i,", i)));
    write(stdout(), to_bytes(format("%i,", flags)));
    write(stdout(), to_bytes(format("%i,", and_ok)));
    write(stdout(), to_bytes(format("%i", add3(x, y, z))));
}
"#,
    );
    assert_eq!(output, "2,7,2,6");
}

/// A local copy must preserve the shared cursor when its temporary store is
/// above the live operand height.
#[test]
fn cursor_safe_copy_propagation_preserves_local_result() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn read_back(int x) -> int {
    let value = 41;
    let copy = value;
    return copy + x;
}
fn main() {
    write(stdout(), to_bytes(format("%i", read_back(1))));
}
"#,
    );
    assert_eq!(output, "42");
}

/// Copied locals feeding MakeArray must not be rewritten into shape-sensitive
/// stack producers that break array construction.
#[test]
fn copy_prop_preserves_array_built_from_copied_locals() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn build() -> int {
    let a = 10;
    let b = a;
    let xs = [b, 20];
    return xs[0] + xs[1];
}
fn main() {
    write(stdout(), to_bytes(format("%i", build())));
}
"#,
    );
    assert_eq!(output, "30");
}

#[test]
fn copy_prop_preserves_spilled_call_result_alias() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn produce() -> int {
    return 41;
}
fn use_copy() -> int {
    let value = produce();
    let copy = value;
    return copy + 1;
}
fn main() {
    write(stdout(), to_bytes(format("%i", use_copy())));
}
"#,
    );
    assert_eq!(output, "42");
}

#[test]
fn copy_prop_preserves_field_aliases() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}
fn sum(Point point) -> int {
    let alias = point;
    return alias.x + alias.y;
}
fn main() {
    write(stdout(), to_bytes(format("%i", sum(new Point(3, 4)))));
}
"#,
    );
    assert_eq!(output, "7");
}

#[test]
fn copy_prop_preserves_match_aliases() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Choice {
    Empty,
    Value(int),
}
fn unwrap(Choice choice) -> int {
    return match choice {
        Choice::Empty => 0,
        Choice::Value(value) => value,
    };
}
fn main() {
    let value = Choice::Value(42);
    let alias = value;
    write(stdout(), to_bytes(format("%i", unwrap(alias))));
}
"#,
    );
    assert_eq!(output, "42");
}

#[test]
fn copy_prop_preserves_loop_carried_writes() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn sum_alias() -> int {
    let seed = 3;
    let alias = seed;
    let i = 0;
    let total = 0;
    while i < 4 {
        total = total + alias;
        i = i + 1;
    }
    return total;
}
fn main() {
    write(stdout(), to_bytes(format("%i", sum_alias())));
}
"#,
    );
    assert_eq!(output, "12");
}

/// MakeEnum one-pass payload build must keep mixed Value/Object order for match.
#[test]
fn make_enum_mixed_payload_survives_match() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Pair {
    Both(int, string),
}
fn describe(Pair p) -> int {
    return match p {
        Pair::Both(n, s) => n + len(s),
    };
}
fn main() {
    write(stdout(), to_bytes(format("%i", describe(Pair::Both(7, "abcd")))));
}
"#,
    );
    assert_eq!(output, "11");
}

#[test]
fn direct_enum_consumers_avoid_heap_construction() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Choice {
    Value(int),
}
enum Point {
    Point { x: int },
}
fn main() {
    let unwrapped = match Choice::Value(42) {
        Choice::Value(value) => value,
    };
    let field = Point::Point { x: 9 }.x;
    write(stdout(), to_bytes(format("%i,%i", unwrapped, field)));
}
"#,
    );
    assert_eq!(output, "42,9");
}

#[test]
fn local_option_match_unbox_runs() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let x = Option::Some(41);
    let y = match x {
        Option::Some(v) => v + 1,
        Option::None => 0,
    };
    let z = match Option::None {
        Option::Some(_) => 9,
        Option::None => 2,
    };
    write(stdout(), to_bytes(format("%i,%i", y, z)));
}
"#,
    );
    assert_eq!(output, "42,2");
}

#[test]
fn local_option_escape_at_call_still_matches() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn take(Option<int> o) -> int {
    return match o {
        Option::Some(v) => v,
        Option::None => 0,
    };
}
fn main() {
    let x = Option::Some(7);
    write(stdout(), to_bytes(format("%i", take(x))));
}
"#,
    );
    assert_eq!(output, "7");
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
    let (bytecode, _) = pipeline.compile_src(src).expect("class field source");
    let symbols = pipeline.program_debug().fn_symbols;
    let main = symbols
        .iter()
        .position(|symbol| symbol.name == "main")
        .expect("main symbol");
    let start = symbols[main].entry_pc as usize;
    let end = symbols
        .get(main + 1)
        .map(|symbol| symbol.entry_pc as usize)
        .unwrap_or(bytecode.len());
    let main_code = &bytecode[start..end];
    assert!(
        main_code.iter().all(|byte| {
            !matches!(
                byte.bytecode(),
                common::Instruction::INIT
                    | common::Instruction::InitTyped
                    | common::Instruction::SetField
                    | common::Instruction::GetField
            )
        }),
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
    let (bytecode, _) = pipeline.compile_src(src).expect("second field temp");
    let symbols = pipeline.program_debug().fn_symbols;
    let main = symbols
        .iter()
        .position(|symbol| symbol.name == "main")
        .expect("main symbol");
    let start = symbols[main].entry_pc as usize;
    let end = symbols
        .get(main + 1)
        .map(|symbol| symbol.entry_pc as usize)
        .unwrap_or(bytecode.len());
    let main_code = &bytecode[start..end];
    assert!(
        main_code.iter().all(|byte| {
            !matches!(
                byte.bytecode(),
                common::Instruction::INIT
                    | common::Instruction::InitTyped
                    | common::Instruction::SetField
                    | common::Instruction::GetField
            )
        }),
        "second-field temp elision must not allocate; opcodes: {:?}",
        main_code
            .iter()
            .map(|b| b.bytecode().mnemonic())
            .collect::<Vec<_>>()
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
    let (bytecode, _) = pipeline.compile_src(src).expect("grouped field temp");
    let symbols = pipeline.program_debug().fn_symbols;
    let main = symbols
        .iter()
        .position(|symbol| symbol.name == "main")
        .expect("main symbol");
    let start = symbols[main].entry_pc as usize;
    let end = symbols
        .get(main + 1)
        .map(|symbol| symbol.entry_pc as usize)
        .unwrap_or(bytecode.len());
    let main_code = &bytecode[start..end];
    assert!(
        main_code.iter().all(|byte| {
            !matches!(
                byte.bytecode(),
                common::Instruction::INIT
                    | common::Instruction::InitTyped
                    | common::Instruction::SetField
                    | common::Instruction::GetField
            )
        }),
        "grouped new Class(args).field must still elide; opcodes: {:?}",
        main_code
            .iter()
            .map(|b| b.bytecode().mnemonic())
            .collect::<Vec<_>>()
    );
}

/// Named locals keep a heap instance when they escape (calls, returns, fields).
/// A unique local that is only field-read may unbox into frame slots.
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
    let (bytecode, _) = pipeline.compile_src(src).expect("named local class");
    let symbols = pipeline.program_debug().fn_symbols;
    let main = symbols
        .iter()
        .position(|symbol| symbol.name == "main")
        .expect("main symbol");
    let start = symbols[main].entry_pc as usize;
    let end = symbols
        .get(main + 1)
        .map(|symbol| symbol.entry_pc as usize)
        .unwrap_or(bytecode.len());
    let main_code = &bytecode[start..end];
    assert!(
        main_code
            .iter()
            .any(|byte| matches!(byte.bytecode(), common::Instruction::InitTyped)),
        "escaping named local must stay InitTyped; opcodes: {:?}",
        main_code
            .iter()
            .map(|b| b.bytecode().mnemonic())
            .collect::<Vec<_>>()
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
    let (bytecode, _) = pipeline.compile_src(src).expect("unbox named local");
    let symbols = pipeline.program_debug().fn_symbols;
    let main = symbols
        .iter()
        .position(|symbol| symbol.name == "main")
        .expect("main symbol");
    let start = symbols[main].entry_pc as usize;
    let end = symbols
        .get(main + 1)
        .map(|symbol| symbol.entry_pc as usize)
        .unwrap_or(bytecode.len());
    let main_code = &bytecode[start..end];
    assert!(
        main_code.iter().all(|byte| {
            !matches!(
                byte.bytecode(),
                common::Instruction::InitTyped | common::Instruction::GetField
            )
        }),
        "non-escaping named local field read should unbox; opcodes: {:?}",
        main_code
            .iter()
            .map(|b| b.bytecode().mnemonic())
            .collect::<Vec<_>>()
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
    let (bytecode, _) = pipeline.compile_src(src).expect("drop temp field");
    let symbols = pipeline.program_debug().fn_symbols;
    let main = symbols
        .iter()
        .position(|symbol| symbol.name == "main")
        .expect("main symbol");
    let start = symbols[main].entry_pc as usize;
    let end = symbols
        .get(main + 1)
        .map(|symbol| symbol.entry_pc as usize)
        .unwrap_or(bytecode.len());
    let main_code = &bytecode[start..end];
    assert!(
        main_code
            .iter()
            .any(|byte| matches!(byte.bytecode(), common::Instruction::InitTyped)),
        "drop class temp must allocate in main so the finalizer can run; opcodes: {:?}",
        main_code
            .iter()
            .map(|b| b.bytecode().mnemonic())
            .collect::<Vec<_>>()
    );
    assert!(
        main_code
            .iter()
            .any(|byte| matches!(byte.bytecode(), common::Instruction::LoadField)),
        "drop class temp must LoadField the heap instance; opcodes: {:?}",
        main_code
            .iter()
            .map(|b| b.bytecode().mnemonic())
            .collect::<Vec<_>>()
    );
}

#[test]
fn class_field_access_emits_load_field_not_get_field() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}
fn main() {
    let p = new Point(1, 2);
    p.x = 3;
    write(stdout(), to_bytes(format("%i", p.x + p.y)));
}
"#;
    let output = run_example_src(src);
    assert_eq!(output, "5");

    let mut pipeline = test_pipeline();
    let (bytecode, _) = pipeline.compile_src(src).expect("class slots");
    assert!(
        bytecode
            .iter()
            .any(|b| matches!(b.bytecode(), common::Instruction::LoadField)),
        "class reads must use LoadField"
    );
    let named_set = bytecode.iter().any(|b| {
        matches!(b.bytecode(), common::Instruction::SetField)
            && common::set_field_slot_index(b.operand_u32()).is_none()
    });
    assert!(
        !named_set,
        "class stores must not use interned-name SetField"
    );
}

#[test]
fn dict_field_access_keeps_interned_get_field() {
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

    let mut pipeline = test_pipeline();
    let (bytecode, _) = pipeline.compile_src(src).expect("dict getfield");
    assert!(
        bytecode
            .iter()
            .any(|b| matches!(b.bytecode(), common::Instruction::GetField)),
        "dict reads must use interned-name GetField"
    );
    assert!(
        bytecode
            .iter()
            .any(|b| matches!(b.bytecode(), common::Instruction::MakeDict)),
        "anonymous record must stay MakeDict"
    );
}

#[test]
fn gc_churn_example_checksum() {
    let src = include_str!("../../examples/perf/gc_churn.hy");
    let output = run_example_src(src);
    assert_eq!(output, "62499500000");
}

#[test]
fn result_heap_churn_example_checksum() {
    let src = include_str!("../../examples/perf/result_heap_churn.hy");
    let output = run_example_src(src);
    assert_eq!(output, "130000000");
}

#[test]
fn host_result_unit_churn_example_checksum() {
    let src = include_str!("../../examples/perf/host_result_unit_churn.hy");
    let output = run_example_src(src);
    assert_eq!(output, "18000000");
}

#[test]
fn pointer_niche_option_match_and_coalesce() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn show(Option<string> value) -> string {
    return match value {
        Option::Some(text) => text,
        Option::None => "none",
    };
}
fn main() {
    write(stdout(), to_bytes(format("%s,%s", show(Option::Some("ok")), show(Option::None))));
}
"#,
    );
    assert_eq!(output, "ok,none");
}

#[test]
fn nested_match_keeps_outer_option_field_bindings() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};

class BoxInt {
    pub opt: Option<int>,
}

class Node {
    pub val: int,
    pub left: Option<Node>,
}

fn nested_boxed(BoxInt b) -> int {
    return match b.opt {
        Option::Some(v) => match b.opt {
            Option::Some(v2) => v + v2,
            Option::None => -1,
        },
        Option::None => 0,
    };
}

fn nested_shadow(BoxInt b) -> int {
    return match b.opt {
        Option::Some(v) => match Option::Some(100) {
            Option::Some(v) => v,
            Option::None => -1,
        },
        Option::None => 0,
    };
}

fn nested_niche(Node n) -> int {
    return match n.left {
        Option::Some(child) => match n.left {
            Option::Some(child2) => child.val + child2.val,
            Option::None => -1,
        },
        Option::None => 0,
    };
}

fn main() {
    let b = new BoxInt(Option::Some(21));
    let leaf = new Node(3, Option::None);
    let root = new Node(1, Option::Some(leaf));
    write(stdout(), to_bytes(format("%i,%i,%i", nested_boxed(b), nested_shadow(b), nested_niche(root))));
}
"#,
    );
    assert_eq!(output, "42,100,6");
}

#[test]
fn nested_match_restores_and_chains_outer_bindings() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};

class BoxInt {
    pub opt: Option<int>,
}

enum Choice {
    A(int),
    B,
}

fn after_nested(BoxInt b) -> int {
    return match b.opt {
        Option::Some(v) => {
            let inner = match Option::Some(1) {
                Option::Some(x) => x,
                Option::None => 0,
            };
            v + inner
        },
        Option::None => 0,
    };
}

fn triple(BoxInt box) -> int {
    return match box.opt {
        Option::Some(a) => match box.opt {
            Option::Some(b) => match box.opt {
                Option::Some(c) => a + b + c,
                Option::None => -1,
            },
            Option::None => -2,
        },
        Option::None => 0,
    };
}

fn nested_result(Result<int, string> r) -> int {
    return match r {
        Result::Ok(v) => match r {
            Result::Ok(v2) => v + v2,
            Result::Err(_) => -1,
        },
        Result::Err(_) => 0,
    };
}

fn nested_choice(Choice c) -> int {
    return match c {
        Choice::A(x) => match c {
            Choice::A(y) => x + y,
            Choice::B => -1,
        },
        Choice::B => 0,
    };
}

fn main() {
    let b = new BoxInt(Option::Some(21));
    let t = new BoxInt(Option::Some(7));
    write(stdout(), to_bytes(format(
        "%i,%i,%i,%i",
        after_nested(b),
        triple(t),
        nested_result(Result::Ok(21)),
        nested_choice(Choice::A(21)),
    )));
}
"#,
    );
    assert_eq!(output, "22,21,42,42");
}

#[test]
fn direct_result_pair_match_and_try() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn parse(int value) {
    if value < 0 {
        raise "bad";
    }
    return value;
}
fn inc(int value) {
    let parsed = parse(value)?;
    return parsed + 1;
}
fn main() {
    let ok = match parse(4) {
        Result::Ok(value) => value,
        Result::Err(_) => -1,
    };
    let bad = match inc(-1) {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };
    let direct_bad = match parse(-1) {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };
    write(
        stdout(),
        to_bytes(format("%i,%i,%i", ok, bad, direct_bad)),
    );
}
"#,
    );
    assert_eq!(output, "4,1,1");
}

#[test]
fn custom_iterator_uses_pointer_niche_option() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
class TextCounter {
    pub cur: int,
    pub end: int,
    pub text: string,
}
impl IntoIterator for TextCounter {
    type Item = string;
    type IntoIter = TextCounter;
    pub fn into_iter(TextCounter value) -> TextCounter {
        return value;
    }
}
impl Iterator for TextCounter {
    type Item = string;
    pub fn next(TextCounter value) -> Option<string> {
        if value.cur < value.end {
            value.cur = value.cur + 1;
            return Option::Some(value.text);
        }
        return Option::None;
    }
}
fn main() {
    let value = new TextCounter(0, 2, "x");
    for text in value {
        write(stdout(), to_bytes(format("%s", text)));
    }
}
"#,
    );
    assert_eq!(output, "xx");
}

/// COI-115: trait instance may call an inherent method on the same type.
#[test]
fn trait_impl_calls_inherent_method_declared_later() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
class ItemBox { pub v: int }
class ItemBoxIter { pub i: int }
impl ItemBox {
    pub fn iter() -> ItemBoxIter {
        return new ItemBoxIter(self.v);
    }
}
impl IntoIterator for ItemBox {
    type Item = int;
    type IntoIter = ItemBoxIter;
    pub fn into_iter(ItemBox m) -> ItemBoxIter {
        return m.iter();
    }
}
impl Iterator for ItemBoxIter {
    type Item = int;
    pub fn next(ItemBoxIter it) -> Option<int> {
        if it.i == 0 {
            it.i = 1;
            return Option::Some(1);
        }
        return Option::None;
    }
}
fn main() {
    let b = new ItemBox(0);
    for x in b {
        write(stdout(), to_bytes(format("%i", x)));
    }
}
"#,
    );
    assert_eq!(output, "1");
}

/// COI-115: static inherent helper used from a trait instance.
#[test]
fn trait_impl_calls_static_inherent_method_declared_later() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
class ItemBox { pub v: int }
class ItemBoxIter { pub i: int }
impl ItemBox {
    pub static fn make_iter(ItemBox m) -> ItemBoxIter {
        return new ItemBoxIter(m.v);
    }
}
impl IntoIterator for ItemBox {
    type Item = int;
    type IntoIter = ItemBoxIter;
    pub fn into_iter(ItemBox m) -> ItemBoxIter {
        return ItemBox::make_iter(m);
    }
}
impl Iterator for ItemBoxIter {
    type Item = int;
    pub fn next(ItemBoxIter it) -> Option<int> {
        if it.i == 0 {
            it.i = 1;
            return Option::Some(7);
        }
        return Option::None;
    }
}
fn main() {
    let b = new ItemBox(0);
    for x in b {
        write(stdout(), to_bytes(format("%i", x)));
    }
}
"#,
    );
    assert_eq!(output, "7");
}

#[test]
fn example_thread_join_prints_42() {
    let output = run_example("examples/thread_join.hy");
    assert_eq!(output, "42");
}

#[test]
fn example_thread_channel_prints_hello() {
    let output = run_example("examples/thread_channel.hy");
    assert_eq!(output, "hello");
}

#[test]
fn example_thread_reply_prints_ping() {
    let output = run_example("examples/thread_reply.hy");
    assert_eq!(output, "ping");
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
    let t = spawn(worker, pair[1])?;
    send(pair[0], "hi")?;
}
"#,
    );
    assert_eq!(output, "hi");
}

#[test]
fn nested_spawn_joins_via_shared_root_registry() {
    // Worker mid spawns leaf on the root Machine's live-thread registry.
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
fn example_thread_mutex_prints_2() {
    let output = run_example("examples/thread_mutex.hy");
    assert_eq!(output, "2");
}

#[test]
fn example_gc_root_weak_prints_pinned() {
    let output = run_example("examples/gc_root_weak.hy");
    assert_eq!(output, "pinned\npinned");
}

#[test]
fn gc_upgrade_some_while_rooted() {
    let output = run_example_src(
        r#"
use gc::{get, root, weak, upgrade};
use io::{stdout, write};
use string::{to_bytes};

fn main() {
    let r = root([7, 8]);
    let inner = match get(r) {
        Option::Some(v) => v,
        Option::None => { return; }
    };
    let w = weak(inner);
    let out = match upgrade(w) {
        Option::Some(_) => "some",
        Option::None => "none",
    };
    write(stdout(), to_bytes(out));
}
"#,
    );
    assert_eq!(output, "some");
}

#[test]
fn example_gc_collect_clears_weak() {
    assert_eq!(run_example("examples/gc_collect.hy"), "none");
}

#[test]
fn example_finalizer_prints_closed() {
    assert_eq!(run_example("examples/finalizer.hy"), "closed");
}

#[test]
fn gc_finalizer_runs_on_collect() {
    let output = run_example_src(
        r#"
use gc::{collect};
use io::{stdout, write};
use string::{format, to_bytes};
static let drops: int = 0;
class Handle { pub fd: int }
impl Handle {
    fn drop() {
        drops = drops + 1;
    }
}
fn make() {
    let h = new Handle(1);
}
fn main() {
    make();
    collect();
    write(stdout(), to_bytes(format("%i", drops)));
}
"#,
    );
    assert_eq!(output, "1");
}

#[test]
fn gc_finalizer_once_bit_and_explicit_drop() {
    let output = run_example_src(
        r#"
use gc::{collect};
use io::{stdout, write};
use string::{format, to_bytes};
static let drops: int = 0;
class Handle { pub fd: int }
impl Handle {
    fn drop() {
        drops = drops + 1;
    }
}
fn main() {
    let h = new Handle(1);
    h.drop();
    h.drop();
    collect();
    write(stdout(), to_bytes(format("%i", drops)));
}
"#,
    );
    assert_eq!(output, "1");
}

#[test]
fn gc_finalizer_skips_live_root() {
    let output = run_example_src(
        r#"
use gc::{collect, root};
use io::{stdout, write};
use string::{format, to_bytes};
static let drops: int = 0;
class Handle { pub fd: int }
impl Handle {
    fn drop() {
        drops = drops + 1;
    }
}
fn main() {
    let h = new Handle(1);
    let r = root(h);
    collect();
    write(stdout(), to_bytes(format("%i", drops)));
}
"#,
    );
    assert_eq!(output, "0");
}

#[test]
fn gc_finalizer_nested_collect_is_deferred() {
    let output = run_example_src(
        r#"
use gc::{collect};
use io::{stdout, write};
use string::{format, to_bytes};
static let drops: int = 0;
class Handle { pub fd: int }
impl Handle {
    fn drop() {
        collect();
        drops = drops + 1;
    }
}
fn make() {
    let h = new Handle(1);
}
fn main() {
    make();
    collect();
    write(stdout(), to_bytes(format("%i", drops)));
}
"#,
    );
    assert_eq!(output, "1");
}

#[test]
fn gc_finalizer_panic_continues_queue() {
    let output = run_example_src(
        r#"
use gc::{collect};
use io::{stdout, write};
use string::{format, to_bytes};
static let drops: int = 0;
class Boom { pub fd: int }
impl Boom {
    fn drop() {
        panic "boom";
    }
}
class Ok { pub fd: int }
impl Ok {
    fn drop() {
        drops = drops + 1;
    }
}
fn make() {
    let a = new Boom(1);
    let b = new Ok(2);
}
fn main() {
    make();
    collect();
    write(stdout(), to_bytes(format("%i", drops)));
}
"#,
    );
    assert!(
        output.ends_with('1'),
        "Ok drop should still run after Boom panics; got {output:?}"
    );
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

#[test]
fn gc_finalizer_can_upgrade_weak_then_clears() {
    let output = run_example_src(
        r#"
use gc::{collect, weak, upgrade, Weak};
use io::{stdout, write};
use string::{format, to_bytes};
class Handle { pub fd: int }
static let during: int = 0;
static let held: Option<Weak<Handle>> = Option::None;
impl Handle {
    fn drop() {
        let live = match held {
            Option::Some(w) => match upgrade(w) {
                Option::Some(_) => 1,
                Option::None => 2,
            },
            Option::None => 3,
        };
        during = live;
    }
}
fn ephemeral() {
    let h = new Handle(1);
    held = Option::Some(weak(h));
}
fn main() {
    ephemeral();
    collect();
    let after = match held {
        Option::Some(w) => match upgrade(w) {
            Option::Some(_) => 1,
            Option::None => 0,
        },
        Option::None => -1,
    };
    write(stdout(), to_bytes(format("%i%i", during, after)));
}
"#,
    );
    assert_eq!(output, "10");
}

#[test]
fn gc_finalizer_storing_self_resurrects_once() {
    // COI-79/COI-95: storing `self` from drop keeps the cell; drop stays once.
    let output = run_example_src(
        r#"
use gc::{collect};
use io::{stdout, write};
use string::{format, to_bytes};
class Handle { pub fd: int }
static let drops: int = 0;
static let kept: Option<Handle> = Option::None;
impl Handle {
    fn drop() {
        drops = drops + 1;
        kept = Option::Some(self);
    }
}
fn make() {
    let h = new Handle(42);
}
fn resurrected_fd() -> int {
    return match kept {
        Option::Some(h) => h.fd,
        Option::None => -1,
    };
}
fn clear_kept() {
    kept = Option::None;
}
fn main() {
    make();
    collect();
    let fd = resurrected_fd();
    let after_first = drops;
    clear_kept();
    collect();
    write(stdout(), to_bytes(format("%i%i%i", after_first, drops, fd)));
}
"#,
    );
    assert_eq!(output, "1142");
}

#[test]
fn gc_explicit_drop_store_self_stays_once() {
    let output = run_example_src(
        r#"
use gc::{collect};
use io::{stdout, write};
use string::{format, to_bytes};
class Handle { pub fd: int }
static let drops: int = 0;
static let kept: Option<Handle> = Option::None;
impl Handle {
    fn drop() {
        drops = drops + 1;
        kept = Option::Some(self);
    }
}
fn explicit() {
    let h = new Handle(7);
    h.drop();
}
fn clear_kept() {
    kept = Option::None;
}
fn main() {
    explicit();
    collect();
    let d1 = drops;
    clear_kept();
    collect();
    write(stdout(), to_bytes(format("%i%i", d1, drops)));
}
"#,
    );
    assert_eq!(output, "11");
}

#[test]
fn gc_finalizer_store_self_into_root_resurrects() {
    // COI-79: root(self) during drop is a documented resurrection edge.
    let output = run_example_src(
        r#"
use gc::{collect, get, root, Root};
use io::{stdout, write};
use string::{format, to_bytes};
class Handle { pub fd: int }
static let drops: int = 0;
static let kept: Option<Root<Handle>> = Option::None;
impl Handle {
    fn drop() {
        drops = drops + 1;
        kept = Option::Some(root(self));
    }
}
fn make() {
    let h = new Handle(42);
}
fn main() {
    make();
    collect();
    let fd = match kept {
        Option::Some(r) => match get(r) {
            Option::Some(h) => h.fd,
            Option::None => -1,
        },
        Option::None => -1,
    };
    let after_first = drops;
    collect();
    let fd2 = match kept {
        Option::Some(r) => match get(r) {
            Option::Some(h) => h.fd,
            Option::None => -1,
        },
        Option::None => -1,
    };
    write(stdout(), to_bytes(format("%i%i%i%i", after_first, drops, fd, fd2)));
}
"#,
    );
    assert_eq!(output, "114242");
}

#[test]
fn gc_finalizer_store_self_into_reachable_field() {
    // COI-79: store into a still-reachable object's field (not only a static).
    let output = run_example_src(
        r#"
use gc::{collect};
use io::{stdout, write};
use string::{format, to_bytes};
class Handle {
    pub fd: int,
}
class Bag {
    pub slot: Option<Handle>,
}
static let drops: int = 0;
static let bag: Option<Bag> = Option::None;
impl Bag {
    pub fn put(Handle h) {
        self.slot = Option::Some(h);
    }
    pub fn fd() -> int {
        return match self.slot {
            Option::Some(h) => h.fd,
            Option::None => -1,
        };
    }
}
impl Handle {
    fn drop() {
        drops = drops + 1;
        match bag {
            Option::Some(b) => b.put(self),
            Option::None => (),
        };
    }
}
fn setup() {
    bag = Option::Some(new Bag(Option::None));
}
fn make() {
    let h = new Handle(9);
}
fn main() {
    setup();
    make();
    collect();
    let fd = match bag {
        Option::Some(b) => b.fd(),
        Option::None => -2,
    };
    let after_first = drops;
    collect();
    write(stdout(), to_bytes(format("%i%i%i", after_first, drops, fd)));
}
"#,
    );
    assert_eq!(output, "119");
}

#[test]
fn gc_finalizer_resurrection_keeps_weak_upgradable() {
    // Re-mark after drop must keep weaks to a resurrected cell live.
    let output = run_example_src(
        r#"
use gc::{collect, weak, upgrade, Weak};
use io::{stdout, write};
use string::{format, to_bytes};
class Handle { pub fd: int }
static let drops: int = 0;
static let kept: Option<Handle> = Option::None;
static let held: Option<Weak<Handle>> = Option::None;
impl Handle {
    fn drop() {
        drops = drops + 1;
        kept = Option::Some(self);
    }
}
fn ephemeral() {
    let h = new Handle(3);
    held = Option::Some(weak(h));
}
fn main() {
    ephemeral();
    collect();
    let after = match held {
        Option::Some(w) => match upgrade(w) {
            Option::Some(h) => h.fd,
            Option::None => -1,
        },
        Option::None => -2,
    };
    write(stdout(), to_bytes(format("%i%i", drops, after)));
}
"#,
    );
    assert_eq!(output, "13");
}

#[test]
fn gc_heap_bytes_is_nonnegative() {
    let output = run_example_src(
        r#"
use gc::{heap_bytes, root};
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let _ = root([1, 2, 3, 4]);
    let n = heap_bytes();
    write(stdout(), to_bytes(format("%z", n >= 0)));
}
"#,
    );
    assert_eq!(output, "true");
}

#[test]
fn gc_collect_preserves_stack_rooted_weak() {
    // `collect` must root the operand stack like auto-GC; a live `Root` on
    // the frame keeps the referent alive so `upgrade` stays `Some`.
    let output = run_example_src(
        r#"
use gc::{collect, get, root, weak, upgrade};
use io::{stdout, write};
use string::{to_bytes};

fn main() {
    let r = root([9, 8, 7]);
    let inner = match get(r) {
        Option::Some(v) => v,
        Option::None => { return; }
    };
    let w = weak(inner);
    let _freed = collect();
    let out = match upgrade(w) {
        Option::Some(_) => "some",
        Option::None => "none",
    };
    write(stdout(), to_bytes(out));
}
"#,
    );
    assert_eq!(output, "some");
}

#[test]
fn gc_collect_returns_nonnegative_freed_bytes() {
    let output = run_example_src(
        r#"
use gc::{collect};
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let n = collect();
    write(stdout(), to_bytes(format("%z", n >= 0)));
}
"#,
    );
    assert_eq!(output, "true");
}

#[test]
fn gc_get_after_unroot_then_collect_clears_weak() {
    let output = run_example_src(
        r#"
use gc::{collect, get, root, unroot, weak, upgrade};
use io::{stdout, write};
use string::{to_bytes};

fn only_weak() {
    let r = root("temp");
    let v = match get(r) {
        Option::Some(x) => x,
        Option::None => { return weak(""); }
    };
    let w = weak(v);
    let _ = unroot(r);
    return w;
}

fn main() {
    let w = only_weak();
    let _ = collect();
    let out = match upgrade(w) {
        Option::Some(_) => "some",
        Option::None => "none",
    };
    write(stdout(), to_bytes(out));
}
"#,
    );
    assert_eq!(output, "none");
}

#[test]
fn thread_channel_close_try_recv_is_disconnected() {
    let output = run_example_src(
        r#"
use thread::{channel, close, try_recv, ThreadError};
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let pair = channel()?;
    let tx = pair[0];
    let rx = pair[1];
    close(tx)?;
    let msg = match try_recv(rx) {
        Result::Ok(_) => "ok",
        Result::Err(e) => match e {
            ThreadError::Disconnected => "disc",
            default => "other",
        },
    };
    write(stdout(), to_bytes(format("%s", msg)));
}
"#,
    );
    assert_eq!(output, "disc");
}

#[test]
fn thread_try_recv_empty_open_channel_would_block() {
    let output = run_example_src(
        r#"
use thread::{channel, try_recv, ThreadError};
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let pair = channel()?;
    let rx = pair[1];
    let msg = match try_recv(rx) {
        Result::Ok(_) => "ok",
        Result::Err(e) => match e {
            ThreadError::WouldBlock => "wb",
            default => "other",
        },
    };
    write(stdout(), to_bytes(format("%s", msg)));
}
"#,
    );
    assert_eq!(output, "wb");
}

#[test]
fn thread_rwlock_with_write_then_read() {
    let output = run_example_src(
        r#"
use thread::{rwlock, with_read, with_write};
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let rw = rwlock(10)?;
    with_write(rw, fn (int n) => (n + 1, 0))?;
    let v = with_read(rw, fn (int n) => n)?;
    write(stdout(), to_bytes(format("%i", v)));
}
"#,
    );
    assert_eq!(output, "11");
}

#[test]
fn thread_detach_then_join_fails() {
    let output = run_example_src(
        r#"
use thread::{detach, join, spawn, ThreadError};
use io::{stdout, write};
use string::{format, to_bytes};

fn work() -> int {
    return 1;
}

fn main() {
    let t = spawn(work)?;
    detach(t)?;
    let msg = match join(t) {
        Result::Ok(_) => "joined",
        Result::Err(e) => match e {
            ThreadError::JoinFailed => "jf",
            default => "other",
        },
    };
    write(stdout(), to_bytes(format("%s", msg)));
}
"#,
    );
    assert_eq!(output, "jf");
}

#[test]
fn example_vec_tuple_prints_zip_broadcast_negate() {
    let output = run_example("examples/vec_tuple.hy");
    assert_eq!(output, "22,23,24,-1-2");
}

#[test]
fn example_vec_packed_mul_uses_hostinvoke_path() {
    let output = run_example("examples/vec_packed_mul.hy");
    assert_eq!(output, "246810121416,3691215182124");
}

#[test]
fn packed_vec_arith_runtime_neg_div_and_scalar_left() {
    // Covers unary neg, float zip div, and non-commutative scalar-left sub
    // on the N≥8 HostInvoke path (mul/broadcast already covered by the example).
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let a = [1, 2, 3, 4, 5, 6, 7, 8];
    let n = -a;
    write(stdout(), to_bytes(format("%i%i,", n[0], n[7])));
    let f = [2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0];
    let d = f / [2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0];
    write(stdout(), to_bytes(format("%f%f,", d[0], d[7])));
    let s = 10 - a;
    write(stdout(), to_bytes(format("%i%i", s[0], s[7])));
}
"#,
    );
    assert_eq!(output, "-1-8,1.08.0,92");
}

#[test]
fn aggregate_float_negate_uses_negf_not_int_neg() {
    // Regression: float aggregate unary `-` must not emit int `NEG`
    // (which bit-twiddles a float as i64). Float path is `NEGF`.
    use common::Instruction;
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let d = -(1.5, 2.0);
    write(stdout(), to_bytes(format("%f,%f", d[0], d[1])));
}
"#;
    let mut pipeline = test_pipeline();
    let (bytecode, constants) = pipeline.compile_src(src).expect("compile");
    assert!(
        bytecode
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::NEGF)),
        "expected NEGF for float aggregate negate"
    );
    assert!(
        !bytecode
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::NEG)),
        "float aggregate negate must not emit int NEG"
    );
    assert_eq!(
        run_bytecode(bytecode, constants, &pipeline, None),
        "-1.5,-2.0"
    );
}

#[test]
fn aggregate_dynamic_array_broadcast_adds_scalar() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn add1([int] xs) -> [int] {
    return xs + 1;
}

fn main() {
    let a = add1([1, 2, 3]);
    write(stdout(), to_bytes(format("%i%i%i", a[0], a[1], a[2])));
}
"#,
    );
    assert_eq!(output, "234");
}

#[test]
fn example_vec_array_prints_zip_broadcast_pow() {
    let output = run_example("examples/vec_array.hy");
    assert_eq!(output, "46,45,18");
}

#[test]
fn example_vec_generic_prints_scale_and_shape_generic_add() {
    let output = run_example("examples/vec_generic.hy");
    assert_eq!(output, "24,55");
}

#[test]
fn example_vec_dot_prints_32_and_cross_product() {
    let output = run_example("examples/vec_dot.hy");
    assert_eq!(output, "32,001");
}

#[test]
fn example_vec_matmul_prints_2x2_product() {
    let output = run_example("examples/vec_matmul.hy");
    assert_eq!(output, "19,22,43,50");
}

#[test]
fn example_matrix_mul_prints_product_and_hadamard_add() {
    let output = run_example("examples/matrix_mul.hy");
    assert_eq!(output, "19,22,43,502");
}

#[test]
fn matrix_compound_add_assign_updates_cells() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let m = matrix([[1, 2], [3, 4]]);
    m += matrix([[10, 20], [30, 40]]);
    write(stdout(), to_bytes(format("%i,%i,%i,%i", m[0][0], m[0][1], m[1][0], m[1][1])));
}
"#,
    );
    assert_eq!(output, "11,22,33,44");
}

#[test]
fn matrix_sub_and_neg_packed_path_values() {
    // End-to-end: Matrix `-` (PackedMatrixZip Sub) and unary `-` (PackedMatrixNeg).
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let a = matrix([[5, 7], [9, 11]]);
    let b = matrix([[1, 2], [3, 4]]);
    let s = a - b;
    write(stdout(), to_bytes(format("%i,%i,%i,%i,", s[0][0], s[0][1], s[1][0], s[1][1])));
    let n = -b;
    write(stdout(), to_bytes(format("%i,%i,%i,%i", n[0][0], n[0][1], n[1][0], n[1][1])));
}
"#,
    );
    assert_eq!(output, "4,5,6,7,-1,-2,-3,-4");
}

#[test]
fn float_dot_packed_path_value() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    write(stdout(), to_bytes(format("%f", dot([1.5, 2.0], [2.5, 4.0]))));
}
"#,
    );
    assert_eq!(output, "11.75");
}

#[test]
fn matmul_tuple_of_tuples_rebuilds_correct_product() {
    // Exercises outer_is_tuple + row_is_tuple packing flags end-to-end.
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let a = ((1, 2), (3, 4));
    let b = ((5, 6), (7, 8));
    let c = matmul(a, b);
    write(stdout(), to_bytes(format("%i,%i,%i,%i", c[0][0], c[0][1], c[1][0], c[1][1])));
}
"#,
    );
    assert_eq!(output, "19,22,43,50");
}

#[test]
fn aggregate_compound_assign_updates_tuple() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let v = (1, 2);
    v += (3, 4);
    write(stdout(), to_bytes(format("%i%i", v[0], v[1])));
}
"#,
    );
    assert_eq!(output, "46");
}

#[test]
fn example_casts_primitive_as_operators() {
    assert_eq!(run_example("examples/casts.hy"), "13true");
}

#[test]
fn example_ansi_color_prints_red() {
    let output = run_example("examples/ansi_color.hy");
    assert!(
        output.contains("red"),
        "expected visible 'red', got {:?}",
        output
    );
}

/// Virtual `time` is gone (COI-259); coil-time is a package.
#[test]
fn virtual_time_module_does_not_resolve() {
    let mut pipeline = test_pipeline();
    let err = pipeline.compile_src("use time::{epoch};\nfn main() {}\n");
    assert!(err.is_err(), "expected module-not-found for virtual time");
    assert!(
        pipeline.messages().iter().any(|m| {
            m.code() == Some(compiler::ErrorCode::IoError)
                || m.message().contains("Module not found")
        }),
        "use time without coil-time must surface Module not found / E0900, got {:?}",
        pipeline.messages()
    );
}

/// Virtual `crypto` is gone (COI-216); coil-crypto is a package.
#[test]
fn virtual_crypto_module_does_not_resolve() {
    let mut pipeline = test_pipeline();
    let err = pipeline.compile_src("use crypto::{sha256};\nfn main() {}\n");
    assert!(err.is_err(), "expected module-not-found for virtual crypto");
    assert!(
        pipeline.messages().iter().any(|m| {
            m.code() == Some(compiler::ErrorCode::IoError)
                || m.message().contains("Module not found")
        }),
        "use crypto without coil-crypto must surface Module not found / E0900, got {:?}",
        pipeline.messages()
    );
}

/// HostInvoke + virtual `clock`: wall/mono advance across a short sleep.
#[test]
fn clock_natives_move_forward_via_host_invoke() {
    let output = run_example_src(
        r#"
use clock::{mono_nanos, sleep_ms, wall_nanos};
use io::{stdout, write};
use string::{format, to_bytes};

fn flag(int later, int earlier) -> int {
    if later > earlier {
        return 1;
    }
    return 0;
}

fn main() {
    let w0 = wall_nanos();
    let m0 = mono_nanos();
    sleep_ms(15);
    let w1 = wall_nanos();
    let m1 = mono_nanos();
    write(stdout(), to_bytes(format("%i%i", flag(w1, w0), flag(m1, m0))));
}
"#,
    );
    assert_eq!(output, "11", "wall and mono must advance, got {output:?}");
}

/// HostInvoke + virtual `io::fs` wiring: `exists(".")` returns Ok.
#[test]
fn fs_exists_dot_ok_via_host_invoke() {
    let output = run_example_src(
        r#"
use io::fs::{exists};
use io::{stdout, write};
use string::{format, to_bytes};

fn dot_ok() -> int {
    return match exists(".") {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
}

fn main() {
    write(stdout(), to_bytes(format("%i", dot_ok())));
}
"#,
    );
    assert_eq!(output, "1");
}

/// HostInvoke + virtual `env` wiring: set/get/remove round-trip.
#[test]
fn env_var_round_trip_via_host_invoke() {
    let output = run_example_src(
        r#"
use env::{remove_var, set_var, var};
use io::{stdout, write};
use string::{format, to_bytes};

fn round_trip() -> string {
    let set_ok = match set_var("COIL_PIPELINE_ENV_KEY", "coil_ok") {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
    if set_ok == 0 {
        return "0";
    }
    let got = match var("COIL_PIPELINE_ENV_KEY") {
        Result::Ok(s) => s,
        Result::Err(_) => "0",
    };
    let rem_ok = match remove_var("COIL_PIPELINE_ENV_KEY") {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
    if rem_ok == 0 {
        return "0";
    }
    return got;
}

fn main() {
    write(stdout(), to_bytes(format("%s", round_trip())));
}
"#,
    );
    assert_eq!(output, "coil_ok");
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

/// Public `tls` / `io::net::tls` / leftover `io::__tls` stay missing.
#[test]
fn virtual_tls_modules_do_not_resolve() {
    fn check_missing(src: &str) {
        let mut pipeline = test_pipeline();
        let err = pipeline.compile_src(src);
        assert!(
            err.is_err(),
            "expected module-not-found, got Ok for {src:?}"
        );
        assert!(
            pipeline.messages().iter().any(|m| {
                m.code() == Some(compiler::ErrorCode::IoError)
                    || m.message().contains("Module not found")
            }),
            "use tls without coil-tls must surface Module not found / E0900, got {:?}",
            pipeline.messages()
        );
    }
    check_missing("use io::net::tls::client::{enable};\nfn main() {}\n");
    check_missing("use io::net::tls::{alpn_protocol};\nfn main() {}\n");
    check_missing("use tls::{client};\nfn main() {}\n");
    check_missing("use io::__tls::client::{enable};\nfn main() {}\n");
    check_missing("use io::__tls::{alpn_protocol};\nfn main() {}\n");
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
fn stream_attach_denied_without_allow_attach() {
    assert_compile_fails(
        r#"
use io::{stdout, attach};
fn main() {
    let s = stdout();
    let _ = attach(s, 0, 0, 0, 0, 0);
}
"#,
        compiler::ErrorCode::HostAttachDenied,
    );
}

#[test]
fn stream_attach_invalid_when_allow_attach() {
    let extra = "";
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
    let output = run_userland_dload_project_grants("allow_attach_null", extra, None, src, &[], grants);
    assert_eq!(
        output, "invalid",
        "attach must reach pointer checks, got {output:?}"
    );
}

#[test]
fn stream_attach_denied_when_only_toml_allows() {
    let extra = "[ffi]\nallow_attach = true\n";
    let src = r#"
use io::{stdout, attach};
fn main() {
    let s = stdout();
    let _ = attach(s, 0, 0, 0, 0, 0);
}
"#;
    assert_dload_project_compile_fails(
        "toml_attach_ignored",
        extra,
        None,
        src,
        &[],
        compiler::ErrorCode::HostAttachDenied,
    );
}

#[test]
fn example_io_tls_does_not_import_virtual_tls() {
    assert_eq!(run_example("examples/io_tls.hy"), "use-coil-tls");
}

/// Feature-off `use` of extracted packages is a compile error (E0900 /
/// "Module not found"), not a hang. Stays ungated so `--no-default-features`
/// still exercises it. Virtual crypto / leftover `io::__tls` / virtual time
/// never resolve.
#[test]
fn optional_virtual_modules_match_cargo_features() {
    fn check(src: &str, enabled: bool) {
        let mut pipeline = test_pipeline();
        let ok = pipeline.compile_src(src).is_ok();
        assert_eq!(
            ok,
            enabled,
            "src={src:?} messages={:?}",
            pipeline.messages()
        );
        if !enabled {
            assert!(
                pipeline.messages().iter().any(|m| {
                    m.code() == Some(compiler::ErrorCode::IoError)
                        || m.message().contains("Module not found")
                }),
                "feature-off use must surface Module not found / E0900, got {:?}",
                pipeline.messages()
            );
        }
    }
    check("use time::{epoch};\nfn main() {}\n", false);
    check("use crypto::{sha256};\nfn main() {}\n", false);
    check("use io::__tls::client::{enable};\nfn main() {}\n", false);
    check("use regex::{compile};\nfn main() {}\n", false);
    check("use io::net::tls::client::{enable};\nfn main() {}\n", false);
    check("use tls::{client};\nfn main() {}\n", false);
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
            !toml.contains("time = "),
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

/// `#[derive(String)]` end-to-end: synthesized `to_string` is callable.
#[test]
fn derive_string_to_string_prints_variant() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
#[derive(String)]
enum Color {
    Red,
}

fn main() {
    write(stdout(), to_bytes(format("%s", Color::Red.to_string())));
}
"#,
    );
    assert_eq!(output, "Color::Red");
}

/// Recursive `#[derive(Hash)]` + primitive Hash instances.
#[test]
fn example_derive_hash_prints_true_true_true_true() {
    let output = run_example("examples/derive_hash.hy");
    assert_eq!(output, "true,true,true,true");
}

/// Primitive `Hash` covers non-int payloads used by derive.
#[test]
fn hash_primitives_and_nested_differ_when_payloads_differ() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
#[derive(Hash)]
enum Box {
    S { s: string },
    B { b: bool },
    F { f: float },
}

fn main() {
    write(stdout(), to_bytes(format("%z,", "a".hash() != "b".hash())));
    write(stdout(), to_bytes(format("%z,", true.hash() != false.hash())));
    write(stdout(), to_bytes(format("%z,", (1.0).hash() != (2.0).hash())));
    write(stdout(), to_bytes(format("%z,", Box::S { s: "x" }.hash() != Box::S { s: "y" }.hash())));
    write(stdout(), to_bytes(format("%z", Box::B { b: true }.hash() != Box::B { b: false }.hash())));
}
"#,
    );
    assert_eq!(output, "true,true,true,true,true");
}

#[test]
fn fallthrough_string_return_requires_explicit_return() {
    let mut pipeline = test_pipeline();
    let err = pipeline.compile_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn bad() -> string {
    // no return
}

fn main() {
    write(stdout(), to_bytes(format("%s", bad())));
}
"#,
    );
    assert!(
        err.is_err(),
        "missing return for string should fail with E0111"
    );
    let msgs = pipeline.messages();
    assert!(
        msgs.iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::ReturnMismatch)),
        "expected ReturnMismatch; got {} messages",
        msgs.len()
    );
}

#[test]
fn fallthrough_unit_allows_implicit_epilogue() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn unitish() {
    let x = 1;
}

fn main() {
    unitish();
    write(stdout(), to_bytes("ok"));
}
"#,
    );
    assert_eq!(output, "ok");
}

#[test]
fn fallthrough_int_requires_explicit_return() {
    let mut pipeline = test_pipeline();
    let err = pipeline.compile_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn answer() -> int {
    // no return
}

fn main() {
    write(stdout(), to_bytes(format("%i", answer())));
}
"#,
    );
    assert!(err.is_err());
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::ReturnMismatch))
    );
}

#[test]
fn fallthrough_bool_byte_float_require_explicit_return() {
    let mut pipeline = test_pipeline();
    let err = pipeline.compile_src(
        r#"
fn flag() -> bool {}
fn b() -> byte {}
fn f() -> float {}
fn main() {}
"#,
    );
    assert!(err.is_err());
    let n = pipeline
        .messages()
        .iter()
        .filter(|m| m.code() == Some(compiler::ErrorCode::ReturnMismatch))
        .count();
    assert!(n >= 3, "expected E0111 for bool/byte/float; got {n}");
}

#[test]
fn fallthrough_option_requires_explicit_return() {
    let mut pipeline = test_pipeline();
    let err = pipeline.compile_src(
        r#"
fn opt() -> Option<int> {}
fn main() { let _ = opt(); }
"#,
    );
    assert!(err.is_err());
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::ReturnMismatch))
    );
}

#[test]
fn fallthrough_async_yield_only_does_not_e0111() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
async fn gen_three() {
    yield 0;
    yield 1;
    yield 2;
}
async fn outer() {
    yield from gen_three();
}
async fn parameterized(int base) {
    yield base;
    yield base + 1;
    yield base + 2;
}
fn main() {
    let _ = resume gen_three();
}
"#,
    );
    assert!(
        result.is_ok(),
        "yield-only async bodies must not E0111 on fall-through: {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| (m.code(), m.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn fallthrough_result_unit_ok_allows_epilogue() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn ok_unit() -> Result<(), string> {}

fn main() {
    write(stdout(), to_bytes(format("%i", match ok_unit() {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    })));
}
"#,
    );
    assert_eq!(output, "1");
}

#[test]
fn fallthrough_result_int_requires_explicit_return() {
    let mut pipeline = test_pipeline();
    let err = pipeline.compile_src(
        r#"
fn ok_int() -> Result<int, string> {}
fn main() { let _ = ok_int(); }
"#,
    );
    assert!(err.is_err());
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::ReturnMismatch))
    );
}

#[test]
fn fallthrough_result_string_and_adt_require_explicit_return() {
    let mut pipeline = test_pipeline();
    let err = pipeline.compile_src(
        r#"
enum Color { Red, Blue }
fn bad_res() -> Result<string, string> {}
fn bad_adt() -> Color {}
fn main() {
    let _ = bad_res();
    let _ = bad_adt();
}
"#,
    );
    assert!(
        err.is_err(),
        "Result<string,_>/ADT fall-through should fail with E0111"
    );
    let msgs = pipeline.messages();
    let mismatches = msgs
        .iter()
        .filter(|m| m.code() == Some(compiler::ErrorCode::ReturnMismatch))
        .count();
    assert!(
        mismatches >= 2,
        "expected ReturnMismatch for both Result<string> and ADT; got {} messages: {:?}",
        msgs.len(),
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
}

#[test]
fn bare_return_semi_exits_unit_fn() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn early(int n) {
    if n == 0 {
        return;
    }
    write(stdout(), to_bytes("go"));
    return;
}

fn main() {
    early(0);
    early(1);
}
"#,
    );
    assert_eq!(output, "go");
}

#[test]
fn while_true_satisfies_non_unit_return() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
fn forever() -> int {
    while true {
    }
}

fn main() {
    let _ = forever;
}
"#,
    );
    assert!(
        result.is_ok(),
        "while true without break should complete -> int: {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
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
fn defer_inside_while_true_warns_e0123() {
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
        "expected DeferNeverRuns inside while true"
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

/// Hard errors must still fail `compile_src` after the warning-only change.
#[test]
fn compile_src_still_fails_on_hard_errors() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
fn main() {
    let _ = totally_undefined_var;
}
"#,
    );
    assert!(result.is_err(), "unknown identifier must fail compile_src");
    assert!(pipeline.had_errors());
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| *m.kind() == reporting::MessageKind::ERROR),
        "expected at least one error: {:?}",
        pipeline.messages()
    );
}

#[test]
fn while_true_with_break_requires_explicit_return() {
    let mut pipeline = test_pipeline();
    let err = pipeline.compile_src(
        r#"
fn f() -> int {
    while true {
        break;
    }
}

fn main() {
    let _ = f;
}
"#,
    );
    assert!(err.is_err(), "break defeats infinite-loop path proof");
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::ReturnMismatch))
    );
}

#[test]
fn const_true_while_satisfies_non_unit_return() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
fn forever() -> int {
    const loop = true;
    while loop {
    }
}

fn main() {
    let _ = forever;
}
"#,
    );
    assert!(
        result.is_ok(),
        "const-folded while cond should complete -> int: {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

#[test]
fn for_true_satisfies_non_unit_return() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
fn forever() -> int {
    for (; true; ) {
    }
}

fn main() {
    let _ = forever;
}
"#,
    );
    assert!(
        result.is_ok(),
        "for (; true; ) without break should complete -> int: {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

#[test]
fn raise_path_satisfies_result_return() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
fn boom() -> Result<int, string> {
    raise "x";
}

fn main() {
    let _ = boom;
}
"#,
    );
    assert!(
        result.is_ok(),
        "raise should count as exit for Result return: {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

#[test]
fn panic_path_satisfies_non_unit_return() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
fn boom() -> int {
    panic "x";
}

fn main() {
    let _ = boom;
}
"#,
    );
    assert!(
        result.is_ok(),
        "panic should count as exit for -> int: {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

#[test]
fn if_without_else_requires_explicit_return() {
    let mut pipeline = test_pipeline();
    let err = pipeline.compile_src(
        r#"
fn f(bool b) -> int {
    if b {
        return 1;
    }
}

fn main() {
    let _ = f;
}
"#,
    );
    assert!(err.is_err(), "if without else must not satisfy -> int");
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::ReturnMismatch))
    );
}

#[test]
fn if_else_returns_satisfy_non_unit_return() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn f(bool b) -> int {
    if b {
        return 1;
    } else {
        return 0;
    }
}

fn main() {
    write(stdout(), to_bytes(format("%i,", f(true))));
    write(stdout(), to_bytes(format("%i", f(false))));
}
"#,
    );
    assert!(
        result.is_ok(),
        "if/else returns should complete -> int: {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

#[test]
fn never_join_match_panic_arm_types_as_int() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn unwrap(Option<int> o) -> int {
    return match o {
        Option::Some(x) => x,
        Option::None => panic "none",
    };
}

fn main() {
    write(stdout(), to_bytes(format("%i", unwrap(Option::Some(7)))));
}
"#,
    );
    assert_eq!(output, "7");
}

#[test]
fn match_all_arms_exit_satisfies_non_unit_return() {
    let mut pipeline = test_pipeline();
    let result = pipeline.compile_src(
        r#"
fn f(Option<int> o) -> int {
    match o {
        Option::Some(x) => panic "some",
        Option::None => panic "none",
    };
}

fn main() {
    let _ = f;
}
"#,
    );
    assert!(
        result.is_ok(),
        "match arms that all exit should complete -> int: {:?}",
        pipeline
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

#[test]
fn unit_fallthrough_still_runs_defers() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn f() {
    defer { write(stdout(), to_bytes("d")); }
    write(stdout(), to_bytes("b"));
}

fn main() {
    f();
}
"#,
    );
    assert_eq!(
        output, "bd",
        "unit epilogue must still emit defers before CONST 0; RETURN"
    );
}

/// Nested multifield record that is not the first outer field must still
/// relocate into scratch and preserve the preceding sibling binding.
#[test]
fn nested_multifield_record_after_sibling_preserves_bindings() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Inner {
    I { x: int, y: int },
}
enum Wrap {
    W { name: int, inner: Inner },
}
fn both(Wrap w) -> int {
    return match w {
        Wrap::W { name, inner: Inner::I { x, y } } => name + x + y,
    };
}
fn main() {
    let w = Wrap::W { name: 3, inner: Inner::I { x: 10, y: 20 } };
    write(stdout(), to_bytes(format("%i", both(w))));
}
"#,
    );
    assert_eq!(
        output, "33",
        "preceding sibling `name` and nested `x`/`y` must all bind"
    );
}

/// `x ** 0/1/2` strength-reduce must still match `i32::pow` (incl. `0 ** 0` → 1).
#[test]
fn int_pow_strength_reduce_prints_correct_values() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let x = 5;
    write(stdout(), to_bytes(format("%i,", x ** 0)));
    write(stdout(), to_bytes(format("%i,", x ** 1)));
    write(stdout(), to_bytes(format("%i,", x ** 2)));
    write(stdout(), to_bytes(format("%i,", x ** 3)));
    write(stdout(), to_bytes(format("%i", 0 ** 0)));
}
"#,
    );
    assert_eq!(output, "1,5,25,125,1");
}

/// `if (!c) { A } else { B }` inverts to `if (c) { B } else { A }` — arms must
/// not swap incorrectly when LogNot is eliminated.
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
    let mut pipeline = test_pipeline();
    let (bytecode, _) = pipeline.compile_src(src).expect("compile");
    assert!(
        !bytecode.iter().any(|b| matches!(
            b.bytecode(),
            common::Instruction::LogNot | common::Instruction::LogNotJmpf
        )),
        "inverted if(!c) else should drop LogNot / LogNotJmpf"
    );
    assert_eq!(run_example_src(src), "10,20");
}

/// SetField leaves the RHS on the stack — statement forms must POP.
/// Const index stores into stack `[T; N]` locals use direct STORE (no StoreIndex).
#[test]
fn set_field_and_store_index_statements_pop_value() {
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
    let mut pipeline = test_pipeline();
    let (bytecode, _) = pipeline.compile_src(src).expect("compile");
    let mut set_field_followed_by_pop = 0usize;
    for w in bytecode.windows(2) {
        if matches!(w[0].bytecode(), common::Instruction::SetField)
            && matches!(w[1].bytecode(), common::Instruction::POP)
        {
            set_field_followed_by_pop += 1;
        }
    }
    assert!(
        set_field_followed_by_pop >= 2,
        "class field assignment statements need SetField; POP"
    );
    // Fixed-array const stores are direct STORE — heap StoreIndex is optional.
    assert_eq!(run_example_src(src), "7,30");
}

/// Repeated GetField of the same name materializes the key once and reuses the value.
#[test]
fn repeated_field_key_reuses_prologue_temp() {
    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}
fn twice(Point p) -> int {
    return p.x + p.x;
}
fn main() {
    write(stdout(), to_bytes(format("%i", twice(new Point(3, 4)))));
}
"#;
    let mut pipeline = test_pipeline();
    let (bytecode, _) = pipeline.compile_src(src).expect("compile");
    // Prologue: STRING table["x"]; STORE temp (ctor also emits STRING "x" for SetField).
    let x_idx = pipeline
        .strings()
        .iter()
        .position(|s| s == "x")
        .expect("string table should contain field key x") as u32;
    let has_string_x_store = bytecode.windows(2).any(|w| {
        matches!(w[0].bytecode(), common::Instruction::STRING)
            && w[0].operand_u32() == x_idx
            && matches!(
                w[1].bytecode(),
                common::Instruction::STORE | common::Instruction::StorePop
            )
    });
    assert!(
        has_string_x_store,
        "p.x + p.x should materialize STRING \"x\" into a temp slot"
    );
    // Value reuse: GetField then DUPLICATE (not a second GetField of x).
    let has_getfield_dup = bytecode.windows(2).any(|w| {
        matches!(w[0].bytecode(), common::Instruction::GetField)
            && matches!(w[1].bytecode(), common::Instruction::DUPLICATE)
    });
    assert!(
        has_getfield_dup,
        "p.x + p.x should GetField once then DUPLICATE"
    );
    assert_eq!(run_example_src(src), "6");
}

/// for-in over arrays hoists ArrayLen out of the loop header.
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
    let (bytecode, _) = pipeline.compile_src(src).expect("compile");
    let syms = pipeline.program_debug().fn_symbols;
    let main_idx = syms.iter().position(|s| s.name == "main").expect("main");
    let start = syms[main_idx].entry_pc as usize;
    let end = syms
        .get(main_idx + 1)
        .map(|s| s.entry_pc as usize)
        .unwrap_or(bytecode.len());
    let lens = bytecode[start..end]
        .iter()
        .filter(|b| matches!(b.bytecode(), common::Instruction::ArrayLen))
        .count();
    assert_eq!(
        lens, 1,
        "main for-in should ArrayLen once before the loop (got {lens})"
    );
    assert!(
        bytecode[start..end]
            .iter()
            .any(|b| matches!(b.bytecode(), common::Instruction::BinSlotSlotJmpf)),
        "loop header should fuse idx < len into BinSlotSlotJmpf"
    );
    assert_eq!(run_example_src(src), "123");
}

/// Option unwrap join-clone + field_hot smoke (GetField / key reuse under load).
#[test]
fn example_perf_field_hot_prints_expected() {
    let output = run_example("examples/perf/field_hot.hy");
    assert_eq!(output, "4000000");
}

#[test]
fn option_unwrap_both_arms_correct_after_return_clone() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn unwrap(Option o) -> int {
    return match o {
        Option::None => 0,
        Option::Some(v) => v,
    };
}
fn main() {
    write(stdout(), to_bytes(format("%i,", unwrap(Option::Some(42)))));
    write(stdout(), to_bytes(format("%i", unwrap(Option::None))));
}
"#,
    );
    assert_eq!(output, "42,0");
}

/// v35 string table: serialize → load → run must keep STRING indexes valid,
/// and the compiler must not emit legacy trailing DATA payloads.
#[test]
fn string_table_archive_round_trip_preserves_literals() {
    use common::{ARCHIVE_VERSION, ArchivedProgram, Instruction};
    use rkyv::rancor::Error;

    let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    write(stdout(), to_bytes(format("%s-%i", "archive", 35)));
}
"#;
    let mut pipeline = test_pipeline();
    let (bytecode, constants) = pipeline.compile_src(src).expect("compile");
    assert!(
        !pipeline.strings().is_empty(),
        "expected program string table entries"
    );
    assert!(
        pipeline.strings().iter().any(|s| s == "archive"),
        "string table should contain literal `archive`: {:?}",
        pipeline.strings()
    );
    assert!(
        bytecode
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::STRING)),
        "expected STRING opcodes indexing the table"
    );
    assert!(
        bytecode
            .iter()
            .all(|b| !matches!(b.bytecode(), Instruction::DATA)),
        "compiler must not emit DATA tombstones after STRING"
    );

    let program = ArchivedProgram {
        version: ARCHIVE_VERSION,
        static_slot_count: pipeline.static_slot_count(),
        constants: constants.clone(),
        strings: pipeline.strings().to_vec(),
        bytecode: bytecode.clone(),
        source_files: pipeline.program_debug().source_files,
        debug_locs: pipeline.program_debug().debug_locs,
        fn_symbols: Vec::new(),
        struct_layouts: Vec::new(),
    };
    let bytes = rkyv::to_bytes::<Error>(&program).expect("serialize");
    let archived =
        rkyv::access::<rkyv::Archived<ArchivedProgram>, Error>(bytes.as_slice()).expect("access");
    assert_eq!(u32::from(archived.version), ARCHIVE_VERSION);
    let loaded_bc: Vec<common::Byte> =
        rkyv::deserialize::<Vec<common::Byte>, Error>(&archived.bytecode).expect("bc");
    let loaded_constants: Vec<u64> =
        rkyv::deserialize::<Vec<u64>, Error>(&archived.constants).expect("consts");
    let loaded_strings: Vec<String> =
        rkyv::deserialize::<Vec<String>, Error>(&archived.strings).expect("strings");
    let static_slots = u32::from(archived.static_slot_count);
    assert!(
        loaded_strings.iter().any(|s| s == "archive"),
        "deserialized string table lost literals: {loaded_strings:?}"
    );

    let shared = SharedBuf::new();
    let mut machine = Machine::<128>::default();
    machine.set_shared_print(shared.inner.clone());
    machine.with_output(shared.clone());
    pipeline.wire_host_natives(&mut machine);
    machine.run_raw(&loaded_bc, &loaded_constants, &loaded_strings, static_slots);
    let _ = machine.restore_output();
    assert_eq!(shared.into_utf8(), "archive-35");
}

/// Worker `write(stdout(), …)` in a multi-iteration loop must keep HostInvoke
/// results on the stack and honor shared-print capture TLS.
#[test]
fn worker_write_all_loop_captures_via_shared_print() {
    let output = run_example_src(
        r#"
use thread::{join, spawn};
use io::{stdout, write};
use string::{format, to_bytes};

fn worker() -> int {
    let i = 0;
    while i < 3 {
        write(stdout(), to_bytes(format("%i", i)));
        i = i + 1;
    }
    return 0;
}

fn main() {
    let t = spawn(worker)?;
    join(t)?;
}
"#,
    );
    assert_eq!(output, "012");
}

/// Qualified `string::format` / `string::to_bytes` resolve without a glob import.
#[test]
fn qualified_string_helpers_without_glob_import() {
    let output = run_example_src(
        r#"
use io::{stdout, write};

fn main() {
    write(stdout(), string::to_bytes(string::format("%s", "ok")));
}
"#,
    );
    assert_eq!(output, "ok");
}

/// Nested HostInvoke call args must stage through temps — evaluating the second
/// `to_bytes` inline must not clobber the first result on the operand stack.
#[test]
fn nested_host_invoke_call_args_stage_without_clobber() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};

fn concat_bytes(Vec<byte> a, Vec<byte> b) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = 0;
    while i < len(a) {
        out.push(a[i]);
        i = i + 1;
    }
    let j = 0;
    while j < len(b) {
        out.push(b[j]);
        j = j + 1;
    }
    return out;
}

fn main() {
    let msg = concat_bytes(to_bytes("GET "), to_bytes("/hi"));
    write(stdout(), msg);
}
"#,
    );
    assert_eq!(output, "GET /hi");
}

#[test]
fn variable_string_add_concatenates() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::to_bytes;

fn main() {
    let a = "GET";
    let b = "/hi";
    write(stdout(), to_bytes(a + b));
}
"#,
    );
    assert_eq!(output, "GET/hi");
}

#[test]
fn variable_string_add_in_test_harness() {
    let src = r#"
use io::{stdout, write};
use string::to_bytes;

test("add") {
    let a = "GET";
    let b = "/hi";
    write(stdout(), to_bytes(a + b));
}
"#;
    let output = run_harness_src(src);
    assert_eq!(output, "GET/hi");
}

/// Nested `+` emits consecutive `"%s%s"` STRING ops. Dup-CSE of those ops
/// broke http `request_build` / multi-piece header lines.
#[test]
fn nested_string_concat_chain_prints_full_content() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::to_bytes;

fn main() {
    let method = "GET";
    let path = "/hi";
    let host = "example.com";
    let line = method + " " + path + " HTTP/1.1\r\nHost: " + host + "\r\n";
    write(stdout(), to_bytes(line));
}
"#,
    );
    assert_eq!(output, "GET /hi HTTP/1.1\r\nHost: example.com\r\n");
}

/// Same nested-concat shape under loop `+=` (format_extra_headers_str style).
#[test]
fn string_plus_equal_loop_builds_header_block() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::to_bytes;

fn main() {
    let names = ["X-Trace", "Accept"];
    let values = ["abc", "text/plain"];
    let acc = names[0] + ": " + values[0] + "\r\n";
    let i = 1;
    while i < 2 {
        acc = acc + names[i] + ": " + values[i] + "\r\n";
        i = i + 1;
    }
    write(stdout(), to_bytes(acc));
}
"#,
    );
    assert_eq!(output, "X-Trace: abc\r\nAccept: text/plain\r\n");
}

/// Bytecode shape: chained concat must keep multiple STRING `"%s%s"` hits
/// (not collapse them via Dup-CSE).
#[test]
fn nested_string_concat_keeps_multiple_pct_s_string_ops() {
    let src = r#"
use io::{stdout, write};
use string::to_bytes;

fn main() {
    let a = "a";
    let b = "b";
    let c = "c";
    write(stdout(), to_bytes(a + b + c));
}
"#;
    let mut pipeline = test_pipeline();
    let (bytecode, _) = pipeline.compile_src(src).expect("compile");
    let pct_idx = pipeline
        .strings()
        .iter()
        .position(|s| s == "%s%s")
        .expect("concat should intern %s%s") as u32;
    let pct_string_ops = bytecode
        .iter()
        .filter(|b| {
            matches!(b.bytecode(), common::Instruction::STRING) && b.operand_u32() == pct_idx
        })
        .count();
    assert!(
        pct_string_ops >= 2,
        "a + b + c should emit ≥2 STRING \"%s%s\" (got {pct_string_ops}); Dup-CSE would under-count"
    );
    // No STRING \"%s%s\" immediately rewritten to DUPLICATE in the final stream.
    let string_then_dup = bytecode.windows(2).any(|w| {
        matches!(w[0].bytecode(), common::Instruction::STRING)
            && w[0].operand_u32() == pct_idx
            && matches!(w[1].bytecode(), common::Instruction::DUPLICATE)
    });
    assert!(
        !string_then_dup,
        "STRING \"%s%s\" must not be followed by DUPLICATE (within-block Dup-CSE)"
    );
    assert_eq!(run_example_src(src), "abc");
}

/// A refused tiny-call inline must leave no bytecode behind.
///
/// `wrap_add`'s body contains an inlinable call, so the inliner starts copying
/// it, reaches the callee's own local (no caller temp) and refuses. The
/// partially copied body used to stay in the buffer and run ahead of the real
/// `CALL`, storing into caller slots — that clobbered the already-computed left
/// operand, so `(1 + 5) + (10 - 3)` printed `10` instead of `13`.
///
/// Only reproduces through the multi-file path the CLI uses; the in-memory
/// single-file compile numbers temps differently and hides the clobber.
#[test]
fn example_inline_wrapped_call_prints_13() {
    assert_eq!(
        run_example("examples/inline_wrapped_call.hy").trim(),
        "13",
        "left operand was clobbered by a leaked inline body"
    );
}

/// Stack-across-CALL for pure non-peel helper arms must keep both results
/// (runtime anchor for `pure_helper_binop_stacks_across_call_for_bin_return`).
#[test]
fn pure_helper_binop_stack_across_call_prints_sum() {
    // left(5) = 0+1+2+3+4 = 10; right(4) = 1+0+1+2+3 = 7; sum = 17
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn left(int n) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        s = s + i;
        i = i + 1;
    }
    return s;
}
fn right(int n) -> int {
    let s = 1;
    let i = 0;
    while i < n {
        s = s + i;
        i = i + 1;
    }
    return s;
}
fn main() {
    write(stdout(), to_bytes(format("%i", left(5) + right(4))));
}
"#,
    );
    assert_eq!(output, "17");
}

/// Tiny-inline binop arms must stage (not bury) — `add(x,y)+add(y,x)` == 14.
#[test]
fn tiny_inline_binop_arms_stage_prints_sum() {
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn add(int a, int b) -> int {
    return a + b;
}
fn main() {
    let x = 3;
    let y = 4;
    write(stdout(), to_bytes(format("%i", add(x, y) + add(y, x))));
}
"#,
    );
    assert_eq!(output, "14");
}

/// Nested call-arg arm must stage so `leaf(leaf(2))+leaf(3)` stays correct.
#[test]
fn nested_call_arg_binop_stages_prints_sum() {
    // leaf(0)=1; leaf(1)=2; leaf(2)=4; leaf(3)=7; leaf(4)=11 → 11+7=18
    let output = run_example_src(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
#[max_depth(16)]
fn leaf(int n) -> int {
    if n <= 0 { return 1; }
    return n + leaf(n - 1);
}
fn main() {
    write(stdout(), to_bytes(format("%i", leaf(leaf(2)) + leaf(3))));
}
"#,
    );
    assert_eq!(output, "18");
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
    let src_path = dir.join("warn_only.hy");
    std::fs::write(
        &src_path,
        r#"
fn main() {
    return;
    let _ = 1;
}
"#,
    )
    .expect("write warn_only.hy");

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

/// COI-16: enum `Construct` with nested `format` must stage the push receiver
/// (len preserved). Nested format-inside-Construct payload correctness is a
/// separate clobber (still open); this only guards the Vec::push drop.
#[test]
fn vec_push_enum_construct_with_format_args_keeps_len() {
    let output = run_example_src(
        r#"
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

enum Row {
    Pair(string, string),
}

fn main() {
    let rows = Vec::new();
    rows.push(Row::Pair(format("a=%s", "1"), format("b=%s", "2")));
    rows.push(Row::Pair(format("c=%s", "3"), format("d=%s", "4")));
    let _ = write_all(stdout(), to_bytes(format("%i", len(rows))));
}
"#,
    );
    assert_eq!(
        output, "2",
        "push with Construct(format,…) must not drop the vec"
    );
}

/// COI-19: heap value in a user static survives `gc::collect` (static roots).
#[test]
fn static_string_survives_gc_collect() {
    let output = run_example_src(
        r#"
use gc::{collect};
use io::{stdout};
use io::sync::{write_all};
use string::{to_bytes};

static const HELD = "across-gc";

fn main() {
    let _freed = collect();
    let _ = write_all(stdout(), to_bytes(HELD));
}
"#,
    );
    assert_eq!(output, "across-gc");
}

/// COI-19: extern of libc is a compile error even with host extra stems.
#[test]
fn extern_strlen_libc_is_compile_error() {
    assert_compile_fails(
        r#"
extern "c" {
    fn strlen(string s) -> int;
}
fn main() {
    let _ = strlen("hello");
}
"#,
        compiler::ErrorCode::HostDloadDenied,
    );
}

/// COI-19: `extern "c"` in an imported module is still a compile error.
#[test]
fn extern_in_imported_module_libc_is_compile_error() {
    let mut pipeline = test_pipeline();
    pipeline.grant_dload_stem("c");
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let full = workspace_root.join("examples/ffi_mod_entry.hy");
    let result = pipeline.compile_src_from_file(full.to_str().unwrap());
    assert!(result.is_err(), "extern c must not compile: {:?}", pipeline.messages());
    assert!(
        pipeline
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::HostDloadDenied)),
        "expected HostDloadDenied, got {:?}",
        pipeline.messages()
    );
}

/// COI-106: binding a unary Result/Option call before match must preserve heap payloads.
#[test]
fn result_bind_then_match_preserves_ok_payload() {
    let bind_fn = run_harness_src(
        r#"
enum Node { Obj { v: int } }
fn make_ok() -> Result<Node, string> { return Node::Obj { v: 42 }; }
test("bind free fn") {
    let r = make_ok();
    let v = match r {
        Result::Ok(n) => match n {
            Node::Obj { v } => v,
        },
        Result::Err(_) => -1,
    };
    assert(v == 42)?;
}
"#,
    );
    assert!(
        !bind_fn.contains("failed"),
        "free fn bind failed: {bind_fn:?}"
    );

    let bind_method = run_harness_src(
        r#"
class Svc {}
enum Node { Obj { v: int } }
impl Svc {
    pub fn decode() -> Result<Node, string> {
        return Node::Obj { v: 42 };
    }
    pub fn fail() -> Result<Node, string> {
        raise "boom";
    }
    pub fn maybe(int flag) -> Option<int> {
        if flag == 0 {
            return Option::None;
        }
        return Option::Some(flag);
    }
}
test("bind method result Ok payload") {
    let s = new Svc();
    let r = s.decode();
    let v = match r {
        Result::Ok(n) => match n {
            Node::Obj { v } => v,
        },
        Result::Err(_) => -1,
    };
    assert(v == 42)?;
}
test("bind method result Err") {
    let s = new Svc();
    let r = s.fail();
    let msg = match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
    assert(msg == "boom")?;
}
test("bind method option Some/None") {
    let s = new Svc();
    let some = s.maybe(7);
    let none = s.maybe(0);
    let v = match some {
        Option::Some(v) => v,
        Option::None => -1,
    };
    let is_none = match none {
        Option::Some(_) => false,
        Option::None => true,
    };
    assert(v == 7)?;
    assert(is_none)?;
}
"#,
    );
    assert!(
        !bind_method.contains("failed"),
        "method call bind+match failed: {bind_method:?}"
    );
}

/// COI-108: `self.inner(x)?` must preserve inner method's Ok payload when the
/// outer function returns a different `Result` payload type.
///
/// Empty SharedBuf alone is not enough: a VM panic from a bad Try shape can
/// leave no `"failed"` banner. Drive cases via `call_function` and require
/// `!panicked && result_is_ok`.
#[test]
fn nested_method_try_preserves_inner_result_payload() {
    let mut pipeline = test_pipeline();
    pipeline.set_include_tests(true);
    let (bytecode, constants) = pipeline
        .compile_src(
            r#"
class Enc {
}
impl Enc {
    pub fn encode(int n) -> Result<Vec<byte>, string> {
        let out: Vec<byte> = Vec::new();
        out.push(n as byte);
        let m = n + 1;
        out.push(m as byte);
        return out;
    }
    pub fn encode_fail(int _n) -> Result<Vec<byte>, string> {
        raise "boom";
    }
    pub fn encode_into(int n) -> Result<int, string> {
        let bytes = self.encode(n)?;
        return len(bytes);
    }
    pub fn encode_first(int n) -> Result<byte, string> {
        let bytes = self.encode(n)?;
        return bytes[0];
    }
    pub fn encode_into_fail(int n) -> Result<int, string> {
        let bytes = self.encode_fail(n)?;
        return len(bytes);
    }
}
fn free_encode(int n) -> Result<Vec<byte>, string> {
    let out: Vec<byte> = Vec::new();
    out.push(n as byte);
    return out;
}
fn free_encode_into(int n) -> Result<int, string> {
    let bytes = free_encode(n)?;
    return len(bytes);
}
test("nested method try keeps vec len") {
    let e = new Enc();
    let n = e.encode_into(10)?;
    assert(n == 2)?;
}
test("nested method try keeps first byte") {
    let e = new Enc();
    let b = e.encode_first(10)?;
    assert(b == (10 as byte))?;
}
test("nested method try propagates Err") {
    let e = new Enc();
    let r = e.encode_into_fail(1);
    let msg = match r {
        Result::Ok(_) => "ok",
        Result::Err(m) => m,
    };
    assert(msg == "boom")?;
}
test("nested free-fn try mismatched Result") {
    let n = free_encode_into(7)?;
    assert(n == 1)?;
}
class Client {
}
impl Client {
    pub fn get() -> Result<int, string> {
        return self.send()?;
    }
    pub fn send() -> Result<int, string> {
        return self.request_send()?;
    }
    pub fn request_send() -> Result<int, string> {
        return 42;
    }
}
test("nested same-Result methods declared later") {
    let c = new Client();
    let n = c.get()?;
    assert(n == 42)?;
}
class ClientFail {
}
impl ClientFail {
    pub fn get() -> Result<int, string> {
        return self.send()?;
    }
    pub fn send() -> Result<int, string> {
        return self.boom()?;
    }
    pub fn boom() -> Result<int, string> {
        raise "nope";
    }
}
test("forward same-Result methods propagate Err") {
    let c = new ClientFail();
    let r = c.get();
    let msg = match r {
        Result::Ok(_) => "ok",
        Result::Err(m) => m,
    };
    assert(msg == "nope")?;
}
class Counter {
}
impl Counter {
    pub fn early() -> int {
        return self.late();
    }
    pub fn late() -> int {
        return 7;
    }
}
test("forward non-Result instance method call") {
    let c = new Counter();
    assert(c.early() == 7)?;
}
class EncFwd {
}
impl EncFwd {
    pub fn encode_into(int n) -> Result<int, string> {
        let bytes = self.encode(n)?;
        return len(bytes);
    }
    pub fn encode(int n) -> Result<Vec<byte>, string> {
        let out: Vec<byte> = Vec::new();
        out.push(n as byte);
        return out;
    }
}
test("forward mismatched-Result method try") {
    let e = new EncFwd();
    let n = e.encode_into(9)?;
    assert(n == 1)?;
}
class Factory {
}
impl Factory {
    pub fn make() -> int {
        return Factory::value();
    }
    pub static fn value() -> int {
        return 9;
    }
}
test("forward static method call from instance") {
    let f = new Factory();
    assert(f.make() == 9)?;
}
"#,
        )
        .expect("COI-108 harness should compile");
    let cases = pipeline.test_cases().to_vec();
    assert_eq!(cases.len(), 9, "expected nine COI-108 cases, got {cases:?}");
    for (name, offset) in &cases {
        let mut machine =
            Machine::<256>::with_operand_capacity(pipeline.operand_stack_slots() as usize);
        pipeline.wire_host_natives(&mut machine);
        machine.load_program(&bytecode, &constants, pipeline.strings());
        let ret = machine.call_function(*offset, &[]);
        assert!(
            !machine.panicked() && machine.result_is_ok(ret),
            "COI-108 case {name:?} failed (panicked={} ret_ok={})",
            machine.panicked(),
            machine.result_is_ok(ret)
        );
    }
}
