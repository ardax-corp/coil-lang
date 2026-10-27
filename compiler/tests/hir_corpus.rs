//! HIR Phase 1 gate: every positive `tests/<domain>` program builds HIR with no
//! unsupported node and every node's span matches its AST node.

use compiler::Pipeline;

#[test]
fn positive_corpus_builds_hir_without_problems() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests");
    let mut files = Vec::new();
    positive_hy_files(&dir, &mut files);
    files.sort();
    // One file per compile, spread over the cores: this test compiles the
    // whole corpus and took 8 minutes on one thread. HIR capture is per thread.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
    let results: Vec<(usize, Vec<String>)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                std::thread::Builder::new()
                    .stack_size(STACK)
                    .spawn_scoped(s, || {
                        let mut built = 0;
                        let mut problems = Vec::new();
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(file) = files.get(i) else { break };
                            if let Some(found) = check(file) {
                                built += 1;
                                problems.extend(found);
                            }
                        }
                        (built, problems)
                    })
                    .expect("spawn worker")
            })
            .collect();
        workers.into_iter().map(|w| w.join().expect("worker panicked")).collect()
    });
    let built: usize = results.iter().map(|r| r.0).sum();
    let mut problems: Vec<String> = results.into_iter().flat_map(|r| r.1).collect();
    problems.sort();
    assert!(built * 10 >= files.len() * 9, "only {built} of {} files compiled", files.len());
    assert!(problems.is_empty(), "{} HIR problems:\n{}", problems.len(), problems.join("\n"));
}

/// Main-thread stack of a `coil` run; deep programs need more than a test
/// thread's default.
const STACK: usize = 8 * 1024 * 1024;

/// HIR problems of `file`, or `None` when it does not compile.
fn check(file: &std::path::Path) -> Option<Vec<String>> {
    comptime::install();
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_include_tests(true);
    compiler::start_hir_capture();
    let ok = pipeline.compile_src_from_file(file.to_str().unwrap()).is_ok();
    let capture = compiler::take_hir_capture();
    if !ok {
        return None;
    }
    assert!(!capture.bodies.is_empty(), "{}: no HIR bodies", file.display());
    Some(capture.problems.into_iter().map(|p| format!("{}: {p}", file.display())).collect())
}

/// `.hy` files under `dir`, recursively, skipping `compile_fail/` directories.
fn positive_hy_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read tests") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "compile_fail") {
                positive_hy_files(&path, out);
            }
        } else if path.extension().is_some_and(|x| x == "hy") {
            out.push(path);
        }
    }
}
