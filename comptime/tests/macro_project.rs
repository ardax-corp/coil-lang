//! `Pipeline::typecheck_project` (the LSP path) sees macro output.

use std::path::PathBuf;

use compiler::Pipeline;

fn temp_hy(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coil_macro_project_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.hy");
    std::fs::write(&file, src).unwrap();
    file
}

#[test]
fn typecheck_project_sees_derive_eq() {
    comptime::install();
    let file = temp_hy(
        "derive_eq",
        "#[derive(Eq)]\nclass Point { pub x: int, pub y: int }\n\nfn eq_ok() -> bool {\n    return new Point(1, 1) == new Point(1, 1);\n}\n\nfn main() {}\n",
    );
    let mut pipeline = Pipeline::new();
    let errors: Vec<String> = pipeline
        .typecheck_project(&file)
        .into_iter()
        .flat_map(|(_, msgs)| msgs)
        .filter(|m| matches!(m.kind(), reporting::MessageKind::ERROR))
        .map(|m| m.message().to_string())
        .collect();
    assert!(errors.is_empty(), "derived Eq should be visible to typecheck_project, got {errors:?}");
}
