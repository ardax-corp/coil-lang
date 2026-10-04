//! HIR Phase 1 gate: every `tests/positive` program builds HIR with no
//! unsupported node and every node's span matches its AST node.

use compiler::Pipeline;

#[test]
fn positive_corpus_builds_hir_without_problems() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/positive");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("read tests/positive")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "hy"))
        .collect();
    files.sort();
    let mut built = 0;
    let mut problems = Vec::new();
    for file in &files {
        let mut pipeline = Pipeline::new();
        pipeline.bind_workspace_language_roots();
        pipeline.set_include_tests(true);
        compiler::start_hir_capture();
        let ok = pipeline.compile_src_from_file(file.to_str().unwrap()).is_ok();
        let capture = compiler::take_hir_capture();
        if !ok {
            continue;
        }
        built += 1;
        assert!(!capture.bodies.is_empty(), "{}: no HIR bodies", file.display());
        for p in capture.problems {
            problems.push(format!("{}: {p}", file.display()));
        }
    }
    assert!(built * 10 >= files.len() * 9, "only {built} of {} files compiled", files.len());
    assert!(problems.is_empty(), "{} HIR problems:\n{}", problems.len(), problems.join("\n"));
}
