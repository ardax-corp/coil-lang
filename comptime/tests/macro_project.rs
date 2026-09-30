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

#[test]
fn debug_locs_for_expanded_macros_point_at_the_use_site() {
    comptime::install();
    let dir = std::env::temp_dir().join(format!(
        "coil_macro_debug_locs_{}",
        std::process::id()
    ));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("gen.hy"),
        "use macro::{Expr, Code};\n\nmacro twice(Expr e) -> Code {\n    return quote expr { ${e} * 2 };\n}\n",
    )
    .unwrap();
    let main = "use gen::{twice};\n\nfn main() {\n    let x = twice!(21);\n    return x;\n}\n";
    std::fs::write(src.join("main.hy"), main).unwrap();
    let mut pipeline = Pipeline::new();
    pipeline.bind_project_root(dir.clone(), vec!["src".into()]);
    pipeline
        .compile_src_from_file(src.join("main.hy").to_str().unwrap())
        .expect("compiles");
    let debug = pipeline.program_debug();
    let file_len = main.len();
    for loc in &debug.debug_locs {
        if !loc.is_known() {
            continue;
        }
        let path = &debug.source_files[loc.file as usize];
        if !path.ends_with("main.hy") {
            continue;
        }
        assert!(
            (loc.start_byte as usize) < file_len,
            "generated loc {}..{} in {path} is past the original file ({file_len} bytes)",
            loc.start_byte,
            loc.end_byte
        );
    }
    let twice_at = main.find("twice!").expect("call") as u32;
    assert!(
        debug.debug_locs.iter().any(|loc| {
            loc.is_known() && loc.start_byte <= twice_at && loc.end_byte > twice_at
        }),
        "expected a debug loc on twice!, got {:?}",
        debug.debug_locs.iter().filter(|l| l.is_known()).collect::<Vec<_>>()
    );
}
