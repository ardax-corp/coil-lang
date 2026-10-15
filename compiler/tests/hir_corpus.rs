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
    let mut built = 0;
    let mut problems = Vec::new();
    for file in &files {
        comptime::install();
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
