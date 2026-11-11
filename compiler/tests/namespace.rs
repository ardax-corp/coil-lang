//! End-to-end tests for `use` / `mod` module resolution.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use compiler::native_mir::{MirAllocKind, MirFunc, MirInst, NativeBody};
use compiler::{CompileFail, HostGrants, Pipeline};
use machine::native::NativeCode;

// Tests change cwd; serialize with CWD_LOCK when running in parallel.

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

/// Manifest with `./src` plus the real workspace stdlib (for `io::sync`, …).
fn manifest_src_and_stdlib() -> String {
    format!(
        "[module]\nroots = [\"./src\", \"{}\"]\n",
        workspace_stdlib().display()
    )
}

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

/// Compile `file` with a capture open, as a run does: without one a compile
/// leaves no bodies.
fn compile_file(pipeline: &mut Pipeline, file: &str) -> Result<Compiled, CompileFail> {
    compiler::native_mir::start_native_capture();
    let constants = pipeline.compile_src_from_file(file);
    let bodies = compiler::native_mir::take_native_capture();
    Ok(Compiled { constants: constants?, bodies })
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

fn run_project(project_root: &Path, entry: &Path) -> String {
    run_project_with_extra(project_root, entry, &[])
}

fn run_project_with_extra(project_root: &Path, entry: &Path, extra: &[PathBuf]) -> String {
    with_project_cwd(project_root, || {
        let mut pipeline = Pipeline::new();
        bind_ns_pipeline(&mut pipeline, extra);
        let compiled = match compile_file(&mut pipeline, entry.to_str().unwrap()) {
            Ok(compiled) => compiled,
            Err(_) => {
                for msg in pipeline.messages() {
                    eprintln!("PIPELINE ERROR: {}", msg.message());
                }
                panic!("compile failed");
            }
        };
        run_compiled(compiled, &pipeline)
    })
}

/// Run `compiled` on the MIR interpreter and return what it printed.
fn run_compiled(compiled: Compiled, pipeline: &Pipeline) -> String {
    let debug = pipeline.program_debug_full();
    let pcs = debug.fn_symbols.iter().map(|s| (s.name.clone(), s.body_id)).collect();
    let interp = coil_mir_interp::prepare(compiled.bodies, &pcs);
    assert!(interp.refused.is_empty(), "the interpreter refused {:?}", interp.refused);
    let code = NativeCode::new(interp.entries, interp.sites).with_complete(true).with_keep(interp.keep);
    machine::native::install_shared_for_next_run(Arc::new(code));
    let program = coil_host::thread_program(
        pipeline,
        coil_host::CompiledProgram {
            constants: &compiled.constants,
            strings: pipeline.strings(),
            static_slots: pipeline.static_slot_count(),
            debug: pipeline.program_debug(),
        },
    );
    let print = Arc::new(Mutex::new(Vec::new()));
    let mut start = coil_host::native_start(pipeline, &pipeline.archived_struct_layouts(), None);
    start.shared_print = Some(Arc::clone(&print));
    machine::run_natively(Arc::new(program), start).expect("the interpreter covers every body");
    machine::io::set_shared_print_redirect(None);
    let bytes = print.lock().expect("print buffer mutex poisoned").clone();
    String::from_utf8(bytes).expect("captured output should be valid UTF-8")
}

/// Serialize cwd changes, `chdir` into `root`, run `f`, then restore cwd.
fn with_project_cwd<R>(root: &Path, f: impl FnOnce() -> R) -> R {
    let _cwd_lock = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

/// Compile `entry` as a project.
fn compile_entry(entry: &Path) -> (Compiled, Pipeline) {
    let mut pipeline = Pipeline::new();
    bind_ns_pipeline(&mut pipeline, &[]);
    let compiled = match compile_file(&mut pipeline, entry.to_str().unwrap()) {
        Ok(compiled) => compiled,
        Err(_) => {
            for msg in pipeline.messages() {
                eprintln!("MSG: {}", msg.message());
            }
            panic!("compile failed");
        }
    };
    (compiled, pipeline)
}

#[test]
fn multiple_roots_search_in_order() {
    let manifest = format!(
        "[module]\nroots = [\"./src\", \"./vendor\", \"{}\"]\n",
        workspace_stdlib().display()
    );
    let files = &[
        ("src/main.coil", "use foo::greet;\nfn main() { greet(); }\n"),
        (
            "src/foo/greet.coil",
            "use io::{stdout, write};\nuse string::{format, to_bytes};\nfn greet() { write(stdout(), to_bytes(format(\"%s\", \"from-src\"))); }\n",
        ),
        (
            "vendor/foo/greet.coil",
            "use io::{stdout, write};\nuse string::{format, to_bytes};\nfn greet() { write(stdout(), to_bytes(format(\"%s\", \"from-vendor\"))); }\n",
        ),
    ];
    let (root, entry) = build_project("multiple_roots", &manifest, files, "src/main.coil");
    let output = run_project_with_extra(&root, &entry, &[root.join("vendor")]);
    assert_eq!(output, "from-src");
}

#[test]
fn no_manifest_uses_default_src_root() {
    let files = &[
        ("src/main.coil", "use foo::greet;\nfn main() { greet(); }\n"),
        (
            "src/foo/greet.coil",
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
    let entry_full = tmp.join("src/main.coil");
    let output = run_project(&tmp, &entry_full);
    assert_eq!(output, "42");
}

/// A generic function's value and a recursive function, linked across two
/// modules.
#[test]
fn two_module_polyfn_and_fib_run() {
    let manifest = manifest_src_and_stdlib();
    // Keep recursive `fib` in the entry namespace (empty prefix) — namespaced
    // modules do not rewrite bare recursive calls to the FQN today.
    let files = &[
        (
            "src/main.coil",
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
            "src/util/inc.coil",
            "fn inc(int x) -> int { return x + 1; }\n",
        ),
    ];
    let (root, entry) = build_project("two_module_polyfn_fib", &manifest, files, "src/main.coil");

    let _cwd_lock = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let compiled = match compile_file(&mut pipeline, entry.to_str().unwrap()) {
        Ok(compiled) => compiled,
        Err(_) => {
            for msg in pipeline.messages() {
                eprintln!("PIPELINE ERROR: {}", msg.message());
            }
            panic!("two-module polyfn+fib compile failed");
        }
    };

    assert!(
        compiled
            .all_insts()
            .any(|i| matches!(i, MirInst::Alloc { kind: MirAllocKind::PolyFn { .. } | MirAllocKind::PolyFnCapture { .. }, .. })),
        "expected a PolyFn value in the linked program"
    );

    let output = run_compiled(compiled, &pipeline);
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
        ("src/main.coil", "mod bad;\nfn main() {}\n"),
        ("src/bad.coil", "@@@ not valid coil\n"),
    ];
    let (root, entry) = build_project("parse_fail_dep", &manifest, files, "src/main.coil");

    let _cwd_lock = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        out.contains("bad.coil"),
        "diagnostic should name the unparseable dependency: {out:?}"
    );
}

#[test]
fn bind_project_root_sets_explicit_roots_without_toml() {
    let files = &[(
        "src/main.coil",
        "use io::{stdout, write};\nuse string::{format, to_bytes};\nfn main() { write(stdout(), to_bytes(format(\"%i\", 42))); }\n",
    )];
    let (root, entry) = build_project("bind_roots", "", files, "src/main.coil");

    let _cwd_lock = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    // The temp dir can be a symlink (macOS), and the root came from the cwd.
    let same = |p: &std::path::Path| p.canonicalize().expect("canonical path");
    assert_eq!(same(pipeline.project_root()), same(&root));
    let compiled = compile_file(&mut pipeline, entry.to_str().unwrap()).expect("bound roots should compile");
    let output = run_compiled(compiled, &pipeline);
    assert_eq!(output, "42");
}

/// A compile does not depend on hash order: clones of another module's
/// methods take their labels, and near the budget their existence, in the
/// order they are requested.
#[test]
fn clones_of_another_modules_methods_compile_the_same_every_time() {
    const METHODS: usize = 12;
    let getters: String = (0..METHODS).map(|i| format!("    pub fn get{i}() -> T {{\n        return self.v;\n    }}\n")).collect();
    let calls: String = (0..METHODS).map(|i| format!("    t = t + b.get{i}().n;\n")).collect();
    let lib = format!("class Box<T> {{\n    pub v: T,\n}}\n\nimpl Box<T> {{\n{getters}}}\n");
    let main = format!("use boxes::Box;\nuse io::{{stdout, write}};\nuse string::{{format, to_bytes}};\n\nclass P {{\n    pub n: int,\n}}\n\nfn main() {{\n    let b = new Box(new P(1));\n    let t = 0;\n{calls}    let _ = write(stdout(), to_bytes(format(\"%i\", t)));\n}}\n");
    let (root, entry) = build_project("clone_order", "", &[("src/boxes.coil", lib.as_str()), ("main.coil", main.as_str())], "main.coil");

    let _cwd_lock = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let original_cwd = std::env::current_dir().expect("get cwd");
    std::env::set_current_dir(&root).expect("chdir");
    struct CwdGuard(PathBuf);
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
    let _guard = CwdGuard(original_cwd);

    let archive = || {
        let mut pipeline = Pipeline::new();
        bind_ns_pipeline(&mut pipeline, &[]);
        let compiled = compile_file(&mut pipeline, entry.to_str().unwrap()).expect("compiles");
        let debug = pipeline.program_debug_full();
        let pcs = debug.fn_symbols.iter().map(|s| (s.name.clone(), s.body_id)).collect();
        compiler::native_mir::archive::encode(&compiled.bodies, &pcs, &[])
    };
    let first = archive();
    for _ in 0..4 {
        assert!(archive() == first, "MIR differs between identical compiles");
    }
}

/// A method of another module, cloned at `int` and at a class of this
/// module: the `int` clone compiles under its own module's context, and the
/// clone of a class that module does not know stays with this one (#2289).
#[test]
fn clones_at_a_class_the_methods_module_does_not_know_stay_here() {
    let lib = "class Box<T> {\n    pub v: T,\n}\n\nimpl Box<T> {\n    pub fn get() -> T {\n        return self.v;\n    }\n}\n";
    let main = r#"use boxes::Box;
use io::{stdout, write};
use string::{format, to_bytes};

class P {
    pub n: int,
}

fn main() {
    let b = new Box(new P(1));
    let s = new Box(7);
    let t = b.get().n + s.get();
    let _ = write(stdout(), to_bytes(format("%i", t)));
}
"#;
    let (root, entry) = build_project("clone_unknown_class", "", &[("src/boxes.coil", lib), ("main.coil", main)], "main.coil");
    assert_eq!(run_project(&root, &entry), "8");
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
            "src/main.coil",
            r#"
use thread::{channel, join, send, spawn, Sender, Thread};
use pool::worker::run_jobs;

class Worker {
    pub thread: Thread,
    pub tx: Sender,
}

impl Worker {
    pub fn submit(string job) {
        match send(self.tx, job) {
            Ok(_) => {},
            Err(_) => panic "send failed",
        }
    }

    pub fn join() {
        join(self.thread)?;
    }
}

fn main() {
    let pair = match channel() {
        Ok(p) => p,
        Err(_) => panic "channel failed",
    };
    let t = spawn(run_jobs, pair[1])?;
    let w = new Worker(t, pair[0]);
    w.submit("a");
    w.submit("b");
    w.submit("stop");
    w.join()?;
}
"#,
        ),
        (
            "src/pool/worker.coil",
            r#"
use thread::{recv, Receiver, ChannelError};
use io::{stdout, write};
use string::{format, to_bytes};

fn run_jobs(Receiver rx) -> Result<int, ChannelError> {
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
    let (root, entry) = build_project("try_pool", &manifest, &files, "src/main.coil");

    let output = with_project_cwd(&root, || {
        let (compiled, pipeline) = compile_entry(&entry);
        run_compiled(compiled, &pipeline)
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
            "src/main.coil",
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
            "src/helper.coil",
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
    let (root, entry) = build_project("io_host_try", &manifest, &files, "src/main.coil");

    let output = with_project_cwd(&root, || {
        let (compiled, pipeline) = compile_entry(&entry);
        run_compiled(compiled, &pipeline)
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
            "src/main.coil",
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
            "src/facade.coil",
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
            "src/transport.coil",
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
    let (root, entry) = build_project("io_host_nested", &manifest, &files, "src/main.coil");

    let output = with_project_cwd(&root, || {
        let (compiled, pipeline) = compile_entry(&entry);
        run_compiled(compiled, &pipeline)
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
            "src/main.coil",
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
    let (root, entry) = build_project("coi233_worker_ffi", "", &files, "src/main.coil");
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
        let compiled = match compile_file(&mut pipeline, entry.to_str().unwrap()) {
            Ok(compiled) => compiled,
            Err(_) => {
                for msg in pipeline.messages() {
                    eprintln!("PIPELINE ERROR: {}", msg.message());
                }
                panic!("compile failed");
            }
        };
        run_compiled(compiled, &pipeline)
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

/// Cross-file: a two-slot `Result<int,int>` helper defined in one module
/// and used as a value in another returns its two words to direct calls;
/// the value enters its one-word adapter (`arith::div$value`), so
/// `CallIndirect` + `match` read the boxed word or, through the adapter's
/// twin, the two words, and never mis-decode the tag.
#[test]
fn two_slot_result_address_taken_in_other_module_enters_its_adapter() {
    let files = [
        (
            "src/arith.coil",
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
            "src/main.coil",
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
    let direct = match div(9, 3) {
        Result::Ok(q) => q,
        Result::Err(_) => 0,
    };
    write(stdout(), to_bytes(format("%i,%i,%i", ok, err, direct)));
}
"#,
        ),
    ];
    let (root, entry) = build_project(
        "two_slot_escape_cross_file",
        &manifest_src_and_stdlib(),
        &files,
        "src/main.coil",
    );
    with_project_cwd(&root, || {
        let mut pipeline = Pipeline::new();
        bind_ns_pipeline(&mut pipeline, &[]);
        // Keeps the direct CALL; the tiny `div` leaf would otherwise inline.
        pipeline.set_inline_max_cost(0);
        let compiled = match compile_file(&mut pipeline, entry.to_str().unwrap()) {
            Ok(compiled) => compiled,
            Err(_) => {
                for msg in pipeline.messages() {
                    eprintln!("PIPELINE ERROR: {}", msg.message());
                }
                panic!("compile failed");
            }
        };
        let body = |name: &str| compiled.bodies.iter().find(|b| b.mir.name == name);
        assert!(
            body("arith::div").is_some_and(|b| b.mir.ret_hi_ty.is_some()),
            "arith::div used as a value should still return two words to direct calls"
        );
        assert!(
            body("arith::div$value").is_some_and(|b| b.mir.ret_hi_ty.is_none()),
            "the value of arith::div should enter its one-word adapter"
        );
        assert!(
            compiled.all_insts().any(|i| matches!(i, MirInst::CallIndirect { .. })),
            "address-of div must lower to an indirect call"
        );
        let output = run_compiled(compiled, &pipeline);
        assert_eq!(output, "5,-1,3");
    });
}

/// Direct CALL across modules (address never taken) still uses two-slot RETURN.
#[test]
fn two_slot_result_direct_cross_module_call_keeps_pair_abi() {
    let files = [
        (
            "src/arith.coil",
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
            "src/main.coil",
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
        "src/main.coil",
    );
    with_project_cwd(&root, || {
        let mut pipeline = Pipeline::new();
        bind_ns_pipeline(&mut pipeline, &[]);
        // Pins the CALL ABI; the tiny `div` leaf would otherwise inline.
        pipeline.set_inline_max_cost(0);
        let compiled = match compile_file(&mut pipeline, entry.to_str().unwrap()) {
            Ok(compiled) => compiled,
            Err(_) => {
                for msg in pipeline.messages() {
                    eprintln!("PIPELINE ERROR: {}", msg.message());
                }
                panic!("compile failed");
            }
        };
        assert!(
            compiled.bodies.iter().any(|b| b.mir.ret_hi_ty.is_some()),
            "direct-only arith::div should keep a two-word return"
        );
        let output = run_compiled(compiled, &pipeline);
        assert_eq!(output, "5");
    });
}

static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
            (format!("src/{n}.coil"), src)
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
    files.push(("src/main.coil".to_string(), main));
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
    let (root, entry) = build_project("module_order", "", &refs, "src/main.coil");

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

/// Compile `files` (entry `src/main.coil`) with `./src` as a root and return
/// the error messages, for programs expected to fail.
fn compile_errors(test_name: &str, files: &[(&str, &str)]) -> Vec<String> {
    let (root, entry) = build_project(test_name, "", files, "src/main.coil");
    let msgs = with_project_cwd(&root, || {
        let mut pipeline = Pipeline::with_reporter(reporting::ReportConfig::default(), Box::new(std::io::sink()));
        bind_ns_pipeline(&mut pipeline, &[root.join("src")]);
        assert!(
            pipeline.compile_src_from_file(entry.to_str().unwrap()).is_err(),
            "{test_name}: expected a compile error"
        );
        pipeline
            .messages()
            .iter()
            .filter(|m| matches!(m.kind(), reporting::MessageKind::ERROR))
            .map(|m| m.message().to_string())
            .collect::<Vec<_>>()
    });
    let _ = std::fs::remove_dir_all(&root);
    msgs
}

/// #2094: a macro provider that does not parse is reported at each use
/// site as the provider's error, the way a provider that does not typecheck
/// is, instead of "macro expansion did not finish after 16 rounds".
#[test]
fn macro_provider_parse_error_is_reported_at_the_use_sites() {
    let msgs = compile_errors(
        "macro_provider_parse_error",
        &[
            (
                "src/mac.coil",
                "use macro::{TypeDecl, Code, raw};\npub trait Broken\nderive Named(TypeDecl t) -> Code { return raw(\"\"); }\nderive Twice(TypeDecl t) -> Code { return raw(\"\"); }\n",
            ),
            (
                "src/main.coil",
                "use mac::Named;\nuse mac::Twice;\n\n#[derive(Named)]\nclass A {\n    pub x: int,\n}\n\n#[derive(Twice)]\nclass B {\n    pub x: int,\n}\n\nfn main() {}\n",
            ),
        ],
    );
    assert!(
        !msgs.iter().any(|m| m.contains("did not finish after")),
        "a provider parse error is not an expansion loop, got: {msgs:?}"
    );
    for name in ["Named", "Twice"] {
        assert!(
            msgs.iter().any(|m| m.contains(&format!("derive `{name}` failed: could not compile the macro module `mac`"))
                && m.contains("`pub` is not allowed on `trait`")),
            "use of `{name}` reports the provider's parse error, got: {msgs:?}"
        );
    }
}

/// #2094: two unresolved item-level macros in one file waited on each
/// other for 16 rounds; nothing ran in a round, so nothing could change.
#[test]
fn two_unresolved_derives_report_unknown_derives_not_an_expansion_loop() {
    let msgs = compile_errors(
        "two_unresolved_derives",
        &[(
            "src/main.coil",
            "#[derive(Named)]\nclass A {\n    pub x: int,\n}\n\n#[derive(Twice)]\nclass B {\n    pub x: int,\n}\n\nfn main() {}\n",
        )],
    );
    assert!(
        !msgs.iter().any(|m| m.contains("did not finish after")),
        "got: {msgs:?}"
    );
    for name in ["Named", "Twice"] {
        assert!(
            msgs.iter().any(|m| m.contains(&format!("`{name}`")) && m.contains("Cannot derive")),
            "`{name}` is reported as unknown, got: {msgs:?}"
        );
    }
}

/// Another module's `entry(k).and_modify(f).or_insert(v)` chain on a view
/// class splices whole into the caller: no view object is built and no
/// view method, `and_modify` (which calls its function parameter) among
/// them, keeps a call (#2238).
#[test]
fn cross_module_entry_chain_dissolves_the_view() {
    let files = [
        (
            "src/counts.coil",
            r#"
class Counts<V> {
    pub keys: Vec<int>,
    pub vals: Vec<V>,
}

view class Slot<V> {
    mut map: Counts<V>,
    key: int,
    slot: int,
}

impl Counts<V> {
    pub fn find(int k) -> int {
        let i = 0;
        while i < self.keys.len() {
            if self.keys[i] == k {
                return i;
            }
            i = i + 1;
        }
        return -1;
    }

    pub fn entry(int k) -> Slot<V> {
        return new Slot(mut self, k, self.find(k));
    }
}

impl Slot<V> {
    pub fn and_modify(V -> V f) -> Slot<V> {
        if self.slot >= 0 {
            self.map.vals[self.slot] = f(self.map.vals[self.slot]);
        }
        return self;
    }

    pub fn or_insert(V v) -> V {
        if self.slot >= 0 {
            return self.map.vals[self.slot];
        }
        self.map.keys.push(self.key);
        self.map.vals.push(v);
        return v;
    }
}
"#,
        ),
        (
            "src/main.coil",
            r#"
use counts::{Counts};

fn main() {
    let c: Counts<int> = new Counts(Vec::new(), Vec::new());
    let i = 0;
    let s = 0;
    while i < 50 {
        s = s + c.entry(i % 7).and_modify(fn (int n) => n + 1).or_insert(1);
        i = i + 1;
    }
    if s != 1029 {
        panic "sum";
    }
}
"#,
        ),
    ];
    assert_entry_chain_dissolves("entry_chain_cross_file", &files);
}

/// The same chain with a generic key, at `string`: the key reaches the
/// view through a copy-on-write op, which does nothing to a string.
#[test]
fn cross_module_entry_chain_dissolves_the_view_with_string_keys() {
    let files = [
        (
            "src/names.coil",
            r#"
class Names<K, V> {
    pub keys: Vec<K>,
    pub vals: Vec<V>,
}

view class Slot<K, V> {
    mut map: Names<K, V>,
    key: K,
    slot: int,
}

impl Names<K: Eq, V> {
    pub fn find(K k) -> int {
        let i = 0;
        while i < self.keys.len() {
            if self.keys[i] == k {
                return i;
            }
            i = i + 1;
        }
        return -1;
    }

    pub fn entry(K k) -> Slot<K, V> {
        return new Slot(mut self, k, self.find(k));
    }
}

impl Slot<K, V> {
    pub fn and_modify(V -> V f) -> Slot<K, V> {
        if self.slot >= 0 {
            self.map.vals[self.slot] = f(self.map.vals[self.slot]);
        }
        return self;
    }

    pub fn or_insert(V v) -> V {
        if self.slot >= 0 {
            return self.map.vals[self.slot];
        }
        self.map.keys.push(self.key);
        self.map.vals.push(v);
        return v;
    }
}
"#,
        ),
        (
            "src/main.coil",
            r#"
use names::{Names};

fn main() {
    let words = ["a", "b", "c", "d", "e", "f", "g"];
    let c: Names<string, int> = new Names(Vec::new(), Vec::new());
    let i = 0;
    let s = 0;
    while i < 50 {
        s = s + c.entry(words[i % 7]).and_modify(fn (int n) => n + 1).or_insert(1);
        i = i + 1;
    }
    if s != 1029 {
        panic "sum";
    }
}
"#,
        ),
    ];
    assert_entry_chain_dissolves("entry_chain_cross_file_string", &files);
}

/// Compiles the project and checks `main` builds no view: `new` of the
/// map is the one object, and the calls left are at most its two
/// `Vec::new()` and `find`, whose loop keeps its call.
fn assert_entry_chain_dissolves(name: &str, files: &[(&str, &str)]) {
    let (root, entry) = build_project(name, &manifest_src_and_stdlib(), files, "src/main.coil");
    with_project_cwd(&root, || {
        let mut pipeline = Pipeline::new();
        bind_ns_pipeline(&mut pipeline, &[]);
        let compiled = match compile_file(&mut pipeline, entry.to_str().unwrap()) {
            Ok(compiled) => compiled,
            Err(_) => {
                for msg in pipeline.messages() {
                    eprintln!("PIPELINE ERROR: {}", msg.message());
                }
                panic!("compile failed");
            }
        };
        let makes = compiled
            .insts("main")
            .filter(|i| matches!(i, MirInst::Alloc { kind: MirAllocKind::Object { .. }, .. }))
            .count();
        let calls = compiled.insts("main").filter(|i| matches!(i, MirInst::Call { .. })).count();
        assert_eq!(makes, 1, "`main` builds the view");
        assert!(calls <= 3, "`main` keeps view method calls: {calls}");
        let _ = run_compiled(compiled, &pipeline);
    });
}

/// The stdlib `HashMap` entry chain splices whole at every level that
/// inlines across modules, not only `-O2`: `main` keeps no call to an
/// `Entry` method or to the lookup the view is built from (#2248).
#[test]
fn stdlib_entry_chain_splices_whole_at_every_level() {
    let files = [(
        "src/main.coil",
        r#"
use collections::map::{HashMap};

fn main() {
    let counts: HashMap<string, int> = HashMap::new();
    let words = ["a", "bb", "ccc", "a", "dd", "bb", "a"];
    let i = 0;
    while i < 70 {
        let w = words[i % 7];
        let _ = counts.entry(w).and_modify(fn (int c) => c + 1).or_insert(1);
        i = i + 1;
    }
    if counts.get("a").unwrap_or(0) != 30 {
        panic "count";
    }
}
"#,
    )];
    let kept = [
        "collections::map::HashMap::entry$inst$string_int",
        "collections::map::Entry::and_modify$inst$string_int",
        "collections::map::Entry::or_insert$inst$string_int",
        "collections::index::KeyIndex::find$inst$string",
    ];
    for level in [compiler::OptLevel::Standard, compiler::OptLevel::Aggressive, compiler::OptLevel::Size] {
        let (root, entry) = build_project(&format!("stdlib_entry_chain_{level:?}"), &manifest_src_and_stdlib(), &files, "src/main.coil");
        with_project_cwd(&root, || {
            let mut pipeline = Pipeline::new();
            bind_ns_pipeline(&mut pipeline, &[]);
            pipeline.set_opt_level(level);
            let compiled = match compile_file(&mut pipeline, entry.to_str().unwrap()) {
                Ok(compiled) => compiled,
                Err(_) => {
                    for msg in pipeline.messages() {
                        eprintln!("PIPELINE ERROR: {}", msg.message());
                    }
                    panic!("compile failed at {level:?}");
                }
            };
            let called: Vec<&str> = compiled
                .insts("main")
                .filter_map(|i| match i {
                    MirInst::Call { target, .. } => compiled.bodies.iter().find(|b| b.entry == Some(*target)),
                    _ => None,
                })
                .map(|b| b.name.as_str())
                .collect();
            for name in kept {
                assert!(!called.contains(&name), "{level:?}: `main` keeps a call to `{name}`");
            }
            let _ = run_compiled(compiled, &pipeline);
        });
    }
}
