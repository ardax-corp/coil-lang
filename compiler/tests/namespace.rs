//! End-to-end tests for `use` / `mod` module resolution.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use compiler::{HostGrants, Pipeline};
use machine::Machine;

// Tests change cwd; serialize with CWD_LOCK when running in parallel.

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

/// Manifest with `./src` plus the real workspace stdlib (for `io::sync`, …).
fn manifest_src_and_stdlib() -> String {
    format!(
        "[module]\nroots = [\"./src\", \"{}\"]\n",
        workspace_stdlib().display()
    )
}

#[derive(Clone, Default)]
struct SharedBuf {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl SharedBuf {
    pub fn new() -> Self {
        Self::default()
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

/// Create a temp project and return `(project_root, entry_path)`.
fn build_project(
    test_name: &str,
    _manifest: &str,
    files: &[(&str, &str)],
    entry: &str,
) -> (PathBuf, PathBuf) {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("coil_ns_test_{}_{}_{}", test_name, pid, nanos));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create temp project dir");

    for (rel, content) in files {
        let full = tmp.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&full, content).expect("write source file");
    }

    let entry_full = tmp.join(entry);
    (tmp, entry_full)
}

fn bind_ns_pipeline(pipeline: &mut Pipeline, extra: &[PathBuf]) {
    let cwd = std::env::current_dir().expect("cwd");
    let mut roots = vec![workspace_stdlib()];
    roots.extend(extra.iter().cloned());
    pipeline.bind_project_roots_with_default(cwd, roots);
    // These tests exercise module resolution, not capabilities.
    pipeline.grant_all();
}

fn run_project(project_root: &PathBuf, entry: &PathBuf) -> String {
    run_project_with_extra(project_root, entry, &[])
}

fn run_project_with_extra(project_root: &PathBuf, entry: &PathBuf, extra: &[PathBuf]) -> String {
    with_project_cwd(project_root, || {
        let mut pipeline = Pipeline::new();
        bind_ns_pipeline(&mut pipeline, extra);
        let (bytecode, constants) = match pipeline.compile_src_from_file(entry.to_str().unwrap()) {
            Ok(pair) => pair,
            Err(_) => {
                for msg in pipeline.messages() {
                    eprintln!("PIPELINE ERROR: {}", msg.message());
                }
                panic!("compile failed");
            }
        };
        run_bytecode(bytecode, constants, &pipeline)
    })
}

fn run_bytecode(bytecode: Vec<common::Byte>, constants: Vec<u64>, pipeline: &Pipeline) -> String {
    let shared = SharedBuf::new();
    let mut machine = Machine::<128>::default();
    machine.with_output(shared.clone());
    // Worker threads print via this buffer (not the parent's Write).
    machine.set_shared_print(shared.inner.clone());
    pipeline.wire_vm_ffi(&mut machine, None);
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
    let bytes = shared
        .inner
        .lock()
        .expect("print buffer mutex poisoned")
        .clone();
    String::from_utf8(bytes).expect("captured output should be valid UTF-8")
}

/// Serialize cwd changes, `chdir` into `root`, run `f`, then restore cwd.
fn with_project_cwd<R>(root: &PathBuf, f: impl FnOnce() -> R) -> R {
    let _cwd_lock = CwdLockGuard(CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner()));
    let original_cwd = std::env::current_dir().expect("get cwd");
    std::env::set_current_dir(root).expect("chdir");
    struct CwdGuard(PathBuf);
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
    let _guard = CwdGuard(original_cwd);
    f()
}

/// Compile `entry` and assert every `JumpIfMatch` pool index is in range.
fn compile_entry_and_assert_jump_if_match_pool_valid(
    entry: &PathBuf,
) -> (Vec<common::Byte>, Vec<u64>, Pipeline) {
    let mut pipeline = Pipeline::new();
    bind_ns_pipeline(&mut pipeline, &[]);
    let (bytecode, constants) = match pipeline.compile_src_from_file(entry.to_str().unwrap()) {
        Ok(pair) => pair,
        Err(_) => {
            for msg in pipeline.messages() {
                eprintln!("MSG: {}", msg.message());
            }
            panic!("compile failed");
        }
    };

    let mut oob = Vec::new();
    for (i, b) in bytecode.iter().enumerate() {
        if matches!(*b.bytecode(), common::Instruction::JumpIfMatch) {
            let idx = (b.operand_u32() & 0xFFFF) as usize;
            if idx >= constants.len() {
                oob.push((i, idx));
            }
        }
    }
    assert!(
        oob.is_empty(),
        "JumpIfMatch pool index out of range after multi-file link: \
         {oob:?} (constants.len() = {})",
        constants.len()
    );
    (bytecode, constants, pipeline)
}

#[test]
fn multiple_roots_search_in_order() {
    let manifest = format!(
        "[module]\nroots = [\"./src\", \"./vendor\", \"{}\"]\n",
        workspace_stdlib().display()
    );
    let files = &[
        ("src/main.hy", "use foo::greet;\nfn main() { greet(); }\n"),
        (
            "src/foo/greet.hy",
            "use io::{stdout, write};\nuse string::{format, to_bytes};\nfn greet() { write(stdout(), to_bytes(format(\"%s\", \"from-src\"))); }\n",
        ),
        (
            "vendor/foo/greet.hy",
            "use io::{stdout, write};\nuse string::{format, to_bytes};\nfn greet() { write(stdout(), to_bytes(format(\"%s\", \"from-vendor\"))); }\n",
        ),
    ];
    let (root, entry) = build_project("multiple_roots", &manifest, files, "src/main.hy");
    let output = run_project_with_extra(&root, &entry, &[root.join("vendor")]);
    assert_eq!(output, "from-src");
}

#[test]
fn no_manifest_uses_default_src_root() {
    let files = &[
        ("src/main.hy", "use foo::greet;\nfn main() { greet(); }\n"),
        (
            "src/foo/greet.hy",
            // No coil.toml → no stdlib root; use virtual `io::write` only.
            "use io::{stdout, write};\nuse string::{format, to_bytes};\nfn greet() { write(stdout(), to_bytes(format(\"%i\", 42))); }\n",
        ),
    ];
    let tmp = std::env::temp_dir().join("coil_ns_test_no_manifest");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create temp dir");
    for (rel, content) in files {
        let full = tmp.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&full, content).expect("write source file");
    }
    let entry_full = tmp.join("src/main.hy");
    let output = run_project(&tmp, &entry_full);
    assert_eq!(output, "42");
}

/// Phase 1: multi-module link finalizes peephole fusion once, relocating
/// `MakePolyFn` while keeping fused fib-style ops in the entry module.
#[test]
fn two_module_polyfn_and_fib_fuse_and_run() {
    use common::Instruction;

    let manifest = manifest_src_and_stdlib();
    // Keep recursive `fib` in the entry namespace (empty prefix) — namespaced
    // modules do not rewrite bare recursive calls to the FQN today.
    let files = &[
        (
            "src/main.hy",
            "use io::{stdout, write};\nuse string::{format, to_bytes};\nuse util::inc;\n\
             fn id<T>(T x) -> T { return x; }\n\
             fn fib(int n) -> int {\n\
               if n <= 2 { return 1; }\n\
               return fib(n - 1) + fib(n - 2);\n\
             }\n\
             fn main() {\n\
               let f = id;\n\
               write(stdout(), to_bytes(format(\"%i\", f(inc(fib(5))))));\n\
             }\n",
        ),
        (
            "src/util/inc.hy",
            "fn inc(int x) -> int { return x + 1; }\n",
        ),
    ];
    let (root, entry) = build_project("two_module_polyfn_fib", &manifest, files, "src/main.hy");

    let _cwd_lock = CwdLockGuard(CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner()));
    let original_cwd = std::env::current_dir().expect("get cwd");
    std::env::set_current_dir(&root).expect("chdir");
    struct CwdGuard(std::path::PathBuf);
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
    let _guard = CwdGuard(original_cwd);

    let mut pipeline = Pipeline::new();
    bind_ns_pipeline(&mut pipeline, &[]);
    let (bytecode, constants) = match pipeline.compile_src_from_file(entry.to_str().unwrap()) {
        Ok(pair) => pair,
        Err(_) => {
            for msg in pipeline.messages() {
                eprintln!("PIPELINE ERROR: {}", msg.message());
            }
            panic!("two-module polyfn+fib compile failed");
        }
    };

    assert!(
        bytecode
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::MakePolyFn)),
        "expected MakePolyFn in linked bytecode"
    );
    let has_fused = bytecode.iter().any(|b| {
        matches!(
            b.bytecode(),
            Instruction::BinSlotImm
                | Instruction::BinSlotImmJmpf
                | Instruction::BinSlotSlot
                | Instruction::CmpJmpf
                |             Instruction::BinReturn
                | Instruction::ConstReturnImm
                | Instruction::LoadReturnSlot
                | Instruction::DenseBin
                | Instruction::DenseBin2
                | Instruction::DenseBinJmpf
        )
    });
    assert!(
        has_fused,
        "final-link fusion should leave fused ops; opcodes: {:?}",
        bytecode.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
    );

    let output = run_bytecode(bytecode, constants, &pipeline);
    // fib(5)=5, inc(5)=6, id(6)=6
    assert_eq!(output, "6");
}

#[test]
fn parse_fail_dependency_emits_single_diagnostic() {
    // discover_all emits parse errors and must not re-enqueue the bad file
    // for compile_file (which would duplicate the same diagnostic).
    use reporting::ReportConfig;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Capture {
        inner: Arc<Mutex<Vec<u8>>>,
    }
    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let manifest = manifest_src_and_stdlib();
    let files = &[
        ("src/main.hy", "mod bad;\nfn main() {}\n"),
        ("src/bad.hy", "@@@ not valid coil\n"),
    ];
    let (root, entry) = build_project("parse_fail_dep", &manifest, files, "src/main.hy");

    let _cwd_lock = CwdLockGuard(CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner()));
    let original_cwd = std::env::current_dir().expect("get cwd");
    std::env::set_current_dir(&root).expect("chdir");
    struct CwdGuard(PathBuf);
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
    let _guard = CwdGuard(original_cwd);

    let capture = Capture::default();
    let mut pipeline = Pipeline::with_reporter(ReportConfig::default(), Box::new(capture.clone()));
    bind_ns_pipeline(&mut pipeline, &[]);
    let result = pipeline.compile_src_from_file(entry.to_str().unwrap());
    assert!(
        result.is_err(),
        "expected compile to fail on bad dependency"
    );
    let _ = pipeline.finish_reporting();
    let out = String::from_utf8_lossy(&capture.inner.lock().unwrap()).into_owned();
    assert!(
        out.contains("Parse error") || out.contains("E0001"),
        "expected parse diagnostic in sink output, got: {out:?}"
    );
    let parse_hits = out.matches("Parse error").count() + out.matches("E0001").count();
    // Pretty sink prints both the code and "Parse error" once per diagnostic.
    assert!(
        parse_hits <= 2,
        "parse diagnostic appears duplicated (discover+compile): hit_count={parse_hits}, out={out:?}"
    );
    assert!(
        out.contains("bad.hy"),
        "diagnostic should name the unparseable dependency: {out:?}"
    );
}

#[test]
fn bind_project_root_sets_explicit_roots_without_toml() {
    let files = &[(
        "src/main.hy",
        "use io::{stdout, write};\nuse string::{format, to_bytes};\nfn main() { write(stdout(), to_bytes(format(\"%i\", 42))); }\n",
    )];
    let (root, entry) = build_project("bind_roots", "", files, "src/main.hy");

    let _cwd_lock = CwdLockGuard(CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner()));
    let original_cwd = std::env::current_dir().expect("get cwd");
    std::env::set_current_dir(&root).expect("chdir");
    struct CwdGuard(PathBuf);
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
    let _guard = CwdGuard(original_cwd);

    let mut pipeline = Pipeline::new();
    bind_ns_pipeline(&mut pipeline, &[]);
    assert_eq!(pipeline.project_root(), root.as_path());
    let (bytecode, constants) = pipeline
        .compile_src_from_file(entry.to_str().unwrap())
        .expect("bound roots should compile");
    let output = run_bytecode(bytecode, constants, &pipeline);
    assert_eq!(output, "42");
}

/// Multi-file programs that use `?` (JumpIfMatch → constant pool) in a
/// dependency must keep a single shared pool across `compile_module`
/// calls. Clearing the pool between files left worker threads panicking
/// at `jump_if_match_target` (pool index OOB) under `spawn`.
#[test]
fn multi_file_try_operator_pool_survives_module_link() {
    let manifest = manifest_src_and_stdlib();
    let files = [
        (
            "src/main.hy",
            r#"
use thread::{channel, join, send, spawn, Sender, Thread};
use pool::worker::run_jobs;

class Worker {
    pub thread: Thread,
    pub tx: Sender,
}

impl Worker {
    pub fn submit(string job) {
        send(self.tx, job)?;
    }

    pub fn join() {
        join(self.thread)?;
    }
}

fn main() {
    let pair = channel()?;
    let t = spawn(run_jobs, pair[1])?;
    let w = new Worker(t, pair[0]);
    w.submit("a")?;
    w.submit("b")?;
    w.submit("stop")?;
    w.join()?;
}
"#,
        ),
        (
            "src/pool/worker.hy",
            r#"
use thread::{recv, Receiver};
use io::{stdout, write};
use string::{format, to_bytes};

fn run_jobs(Receiver rx) -> Result<int, ThreadError> {
    while true {
        let job = recv(rx)?;
        if job == "stop" {
            break;
        }
        write(stdout(), to_bytes(format("%s,", job)));
    }
    return 0;
}
"#,
        ),
    ];
    let (root, entry) = build_project("try_pool", &manifest, &files, "src/main.hy");

    let output = with_project_cwd(&root, || {
        let (bytecode, constants, pipeline) =
            compile_entry_and_assert_jump_if_match_pool_valid(&entry);
        run_bytecode(bytecode, constants, &pipeline)
    });
    assert_eq!(output, "a,b,");
}

/// Dependency modules that call IO HostInvoke + `?` must share the same
/// constant pool as the entry (same class of bug as
/// `multi_file_try_operator_pool_survives_module_link`, but with real Stream
/// IO rather than thread channels).
#[test]
fn multi_file_io_hostinvoke_try_in_dependency() {
    let manifest = manifest_src_and_stdlib();
    let files = [
        (
            "src/main.hy",
            r#"
use io::{stdout, write};

use helper::write_greeting;
use string::{to_bytes};

fn main() {
    write_greeting()?;
    write(stdout(), to_bytes("ok"));
}
"#,
        ),
        (
            "src/helper.hy",
            r#"
use io::{close, open, write, IoError};

fn write_greeting() -> Result<(), IoError> {
    let path = "greeting.bin";
    let h: byte = 104;
    let i: byte = 105;
    let nl: byte = 10;
    let payload = Vec::from([h, i, nl]);
    let w = open(path, "w")?;
    write(w, payload)?;
    close(w)?;
    return ();
}
"#,
        ),
    ];
    let (root, entry) = build_project("io_host_try", &manifest, &files, "src/main.hy");

    let output = with_project_cwd(&root, || {
        let (bytecode, constants, pipeline) =
            compile_entry_and_assert_jump_if_match_pool_valid(&entry);
        run_bytecode(bytecode, constants, &pipeline)
    });
    assert_eq!(output, "ok");

    let written = std::fs::read(root.join("greeting.bin")).expect("dependency wrote greeting.bin");
    assert_eq!(written, b"hi\n");
}

/// Nested library layout (entry → facade → transport): deepest module owns
/// Stream IO + `?`. Models stdlib/http calling a transport helper while the
/// shared constant pool must survive every `compile_module` hop.
#[test]
fn multi_file_io_hostinvoke_try_in_nested_dependency() {
    let manifest = manifest_src_and_stdlib();
    let files = [
        (
            "src/main.hy",
            r#"
use facade::roundtrip;
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    roundtrip()?;
    write(stdout(), to_bytes("ok"));
}
"#,
        ),
        (
            "src/facade.hy",
            r#"
use io::{IoError};
use transport::write_then_read;

fn roundtrip() -> Result<(), IoError> {
    write_then_read("nested.bin")?;
    return ();
}
"#,
        ),
        (
            "src/transport.hy",
            r#"
use io::{close, open, write, IoError};
use io::sync::{read_exact};

fn write_then_read(string path) -> Result<(), IoError> {
    let a: byte = 65;
    let b: byte = 66;
    let payload = Vec::from([a, b]);
    let w = open(path, "w")?;
    write(w, payload)?;
    close(w)?;

    let r = open(path, "r")?;
    let z: byte = 0;
    let out = Vec::from([z, z]);
    read_exact(r, out)?;
    close(r)?;
    return ();
}
"#,
        ),
    ];
    let (root, entry) = build_project("io_host_nested", &manifest, &files, "src/main.hy");

    let output = with_project_cwd(&root, || {
        let (bytecode, constants, pipeline) =
            compile_entry_and_assert_jump_if_match_pool_valid(&entry);
        run_bytecode(bytecode, constants, &pipeline)
    });
    assert_eq!(output, "ok");

    let written = std::fs::read(root.join("nested.bin")).expect("transport wrote nested.bin");
    assert_eq!(written, b"AB");
}

/// Soft-skip an FFI-dependent test outside CI. In CI (`CI` env set), skip is a
/// hard failure so missing `cc` / libffi never silently greens the suite.
fn ffi_soft_skip(reason: &str) {
    if std::env::var_os("CI").is_some() {
        panic!("FFI soft-skip forbidden in CI: {reason}");
    }
    eprintln!("skipping: {reason}");
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate parent")
        .to_path_buf()
}

/// Build `examples/sum.c` into `dir` as the platform `libsum` filename.
fn build_libsum_into(dir: &Path) -> Option<PathBuf> {
    let src = workspace_root().join("examples/sum.c");
    if !src.exists() {
        ffi_soft_skip(&format!("{} missing", src.display()));
        return None;
    }
    std::fs::create_dir_all(dir).expect("allowed ffi dir");
    let dest = dir.join(machine::platform_shared_lib_filename("sum"));
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
    match cmd.arg("-O2").arg("-o").arg(&dest).arg(&src).status() {
        Ok(s) if s.success() && dest.exists() => Some(dest),
        Ok(s) => {
            ffi_soft_skip(&format!("cc status {}", s.code().unwrap_or(-1)));
            None
        }
        Err(e) => {
            ffi_soft_skip(&format!("cc: {e}"));
            None
        }
    }
}

/// COI-233 worker fixture: FFI search dir `./allowed` with libsum
/// there, and a non-loadable marker at cwd `native/libtls.so` (not a hijack).
fn run_coi233_worker_ffi_project() -> Option<String> {
    let files = [
        (
            "src/main.hy",
            r#"
use ffi::{dload};
use thread::{join, spawn};
use io::{stdout, write};
use string::{format, to_bytes};

fn worker() -> int {
    let tls = match dload("tls") {
        Result::Ok(_) => "tls-loaded",
        Result::Err(e) => e.message,
    };
    write(stdout(), to_bytes(tls));
    let sum_ok = match dload("sum") {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
    return sum_ok;
}

fn main() {
    let parent = match dload("sum") {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
    let t = spawn(worker)?;
    let w = join(t)?;
    write(stdout(), to_bytes(format("|%i%i", parent, w)));
}
"#,
        ),
        (
            "native/libtls.so",
            "coil-security-pin: not a shared library\n",
        ),
    ];
    let (root, entry) = build_project("coi233_worker_ffi", "", &files, "src/main.hy");
    let Some(libsum) = build_libsum_into(&root.join("allowed")) else {
        let _ = std::fs::remove_dir_all(&root);
        return None;
    };
    let output = with_project_cwd(&root, || {
        let mut pipeline = Pipeline::new();
        bind_ns_pipeline(&mut pipeline, &[]);
        let mut grants = HostGrants::deny_all();
        grants.add_ffi_search_path(root.join("allowed"));
        grants.grant_dload_allow("sum");
        grants.grant_dload_allow("tls");
        pipeline.set_host_grants(grants);
        pipeline.grant_dload_file("sum", libsum);
        let (bytecode, constants) = match pipeline.compile_src_from_file(entry.to_str().unwrap()) {
            Ok(pair) => pair,
            Err(_) => {
                for msg in pipeline.messages() {
                    eprintln!("PIPELINE ERROR: {}", msg.message());
                }
                panic!("compile failed");
            }
        };
        run_bytecode(bytecode, constants, &pipeline)
    });
    let _ = std::fs::remove_dir_all(&root);
    Some(output)
}

/// COI-233: a worker `dload("tls")` must not take cwd `./native/libtls`.
#[test]
fn worker_dload_does_not_search_cwd_native_tls() {
    let Some(output) = run_coi233_worker_ffi_project() else {
        return;
    };
    let lower = output.to_ascii_lowercase();
    assert!(
        !lower.contains("tls-loaded"),
        "worker must not load cwd ./native/ as a tls fallback, got {output:?}"
    );
    assert!(
        !lower.contains("native/libtls") && !lower.contains("native\\libtls"),
        "worker dload must not search cwd ./native/libtls, got {output:?}"
    );
    assert!(
        output.ends_with("|10") || output.ends_with("|11"),
        "parent must dload sum via FFI search paths, got {output:?}"
    );
}

/// COI-233: a `thread::spawn` worker must resolve `dload("sum")` from the same
/// FFI search paths as the root graph.
#[test]
fn worker_dload_sees_parent_ffi_search_paths() {
    let Some(output) = run_coi233_worker_ffi_project() else {
        return;
    };
    assert!(
        output.ends_with("|11"),
        "parent and worker must both dload sum via FFI search paths, got {output:?}"
    );
}

/// Cross-file hole: defining a two-slot `Result<int,int>` helper in one
/// module and taking its address in another must keep the boxed ABI so
/// `CallIndirect` + `match` cannot stack-smash / mis-decode the tag.
#[test]
fn two_slot_result_address_taken_in_other_module_stays_boxed() {
    let files = [
        (
            "src/arith.hy",
            r#"
fn div(int a, int b) -> Result<int, int> {
    if b == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(a / b);
}
"#,
        ),
        (
            "src/main.hy",
            r#"
use arith::{div};
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let f = div;
    let ok = match f(10, 2) {
        Result::Ok(q) => q,
        Result::Err(_) => 0,
    };
    let err = match f(1, 0) {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    };
    write(stdout(), to_bytes(format("%i,%i", ok, err)));
}
"#,
        ),
    ];
    let (root, entry) = build_project(
        "two_slot_escape_cross_file",
        &manifest_src_and_stdlib(),
        &files,
        "src/main.hy",
    );
    with_project_cwd(&root, || {
        let mut pipeline = Pipeline::new();
        bind_ns_pipeline(&mut pipeline, &[]);
        let (bytecode, constants) = match pipeline.compile_src_from_file(entry.to_str().unwrap()) {
            Ok(pair) => pair,
            Err(_) => {
                for msg in pipeline.messages() {
                    eprintln!("PIPELINE ERROR: {}", msg.message());
                }
                panic!("compile failed");
            }
        };
        let has_two_word_return = bytecode.iter().any(|b| {
            *b.bytecode() == common::Instruction::RETURN && b.return_words() == 2
        });
        assert!(
            !has_two_word_return,
            "escaped arith::div must keep boxed RETURN, found RETURN operand 2"
        );
        let has_call_indirect = bytecode
            .iter()
            .any(|b| *b.bytecode() == common::Instruction::CallIndirect);
        assert!(
            has_call_indirect,
            "address-of div must lower to CallIndirect"
        );
        let output = run_bytecode(bytecode, constants, &pipeline);
        assert_eq!(output, "5,-1");
    });
}

/// Direct CALL across modules (address never taken) still uses two-slot RETURN.
#[test]
fn two_slot_result_direct_cross_module_call_keeps_pair_abi() {
    let files = [
        (
            "src/arith.hy",
            r#"
fn div(int a, int b) -> Result<int, int> {
    if b == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(a / b);
}
"#,
        ),
        (
            "src/main.hy",
            r#"
use arith::{div};
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let ok = match div(10, 2) {
        Result::Ok(q) => q,
        Result::Err(_) => 0,
    };
    write(stdout(), to_bytes(format("%i", ok)));
}
"#,
        ),
    ];
    let (root, entry) = build_project(
        "two_slot_direct_cross_file",
        &manifest_src_and_stdlib(),
        &files,
        "src/main.hy",
    );
    with_project_cwd(&root, || {
        let mut pipeline = Pipeline::new();
        bind_ns_pipeline(&mut pipeline, &[]);
        // Pins the CALL ABI; the tiny `div` leaf would otherwise inline.
        pipeline.set_inline_max_cost(0);
        let (bytecode, constants) = match pipeline.compile_src_from_file(entry.to_str().unwrap()) {
            Ok(pair) => pair,
            Err(_) => {
                for msg in pipeline.messages() {
                    eprintln!("PIPELINE ERROR: {}", msg.message());
                }
                panic!("compile failed");
            }
        };
        let has_two_word_return = bytecode.iter().any(|b| {
            *b.bytecode() == common::Instruction::RETURN && b.return_words() == 2
        });
        assert!(
            has_two_word_return,
            "direct-only arith::div should keep two-slot RETURN"
        );
        let output = run_bytecode(bytecode, constants, &pipeline);
        assert_eq!(output, "5");
    });
}

static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct CwdLockGuard(std::sync::MutexGuard<'static, ()>);
impl Drop for CwdLockGuard {
    fn drop(&mut self) {}
}

/// #605: modules with no `use` edge between them were ordered by a
/// `HashMap`'s iteration, so the same project linked its functions in a
/// different order on every compile. Each `Pipeline` hashes with fresh
/// keys, so a few compiles in one process expose any such order.
#[test]
fn independent_modules_link_in_the_same_order_every_compile() {
    let names = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];
    let mut files: Vec<(String, String)> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let class = format!("{}{}", n[..1].to_uppercase(), &n[1..]);
            let src = format!(
                "class {class} {{\n    pub v: int,\n}}\n\nimpl {class} {{\n    pub static fn make() -> {class} {{\n        return new {class}({i});\n    }}\n\n    pub fn get() -> int {{\n        return self.v;\n    }}\n}}\n\nfn {n}_value() -> int {{\n    return {class}::make().get();\n}}\n"
            );
            (format!("src/{n}.hy"), src)
        })
        .collect();
    let mut main = String::new();
    for n in names {
        main.push_str(&format!("use {n}::{{{n}_value}};\n"));
    }
    main.push_str("\nfn main() {\n    let total = 0;\n");
    for n in names {
        main.push_str(&format!("    total = total + {n}_value();\n"));
    }
    main.push_str("}\n");
    files.push(("src/main.hy".to_string(), main));
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
    let (root, entry) = build_project("module_order", "", &refs, "src/main.hy");

    let compile = || {
        with_project_cwd(&root, || {
            let mut pipeline = Pipeline::new();
            bind_ns_pipeline(&mut pipeline, &[root.join("src")]);
            pipeline
                .compile_src_from_file(entry.to_str().unwrap())
                .unwrap_or_else(|_| panic!("compile failed: {:?}", pipeline.messages()))
        })
    };
    let first = compile();
    for run in 1..6 {
        assert!(compile() == first, "compile {run} linked differently from compile 0");
    }
    let _ = std::fs::remove_dir_all(&root);
}
