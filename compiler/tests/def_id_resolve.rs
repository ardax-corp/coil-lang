//! B1: interned DefIds are stable across `use` of a top-level fn.

use std::path::PathBuf;

use compiler::Pipeline;

fn build_project(test_name: &str, files: &[(&str, &str)], entry: &str) -> (PathBuf, PathBuf) {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("coil_defid_{test_name}_{pid}_{nanos}"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create temp project dir");
    std::fs::write(tmp.join("coil.toml"), "[module]\nroots = [\"./src\"]\n")
        .expect("write coil.toml");
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

#[test]
fn imported_fn_def_id_matches_defining_module() {
    let files = [
        (
            "src/math.hy",
            "fn add(int a, int b) -> int {\n    return a + b;\n}\n",
        ),
        (
            "src/main.hy",
            "use math::add;\nfn main() {\n    add(1, 2);\n}\n",
        ),
    ];
    let (_root, entry) = build_project("use_same_id", &files, "src/main.hy");
    let mut pipeline = Pipeline::new();
    if let Err(()) = pipeline.compile_src_from_file(entry.to_str().unwrap()) {
        for msg in pipeline.messages() {
            eprintln!("PIPELINE ERROR: {}", msg.message());
        }
        panic!("compile failed");
    }
    let checker = pipeline.compiler().checker();
    let imported = checker
        .def_id_of("add")
        .expect("imported `add` should have a DefId");
    let defined = checker
        .interned_def("math", "add")
        .expect("defining module `math::add` should be interned");
    assert_eq!(
        imported, defined,
        "use math::add must bind the defining DefId"
    );
    // Distinct top-level fns intern to distinct ids.
    let main_id = checker
        .def_id_of("main")
        .expect("entry `main` should have a DefId");
    assert_ne!(imported, main_id);
}
