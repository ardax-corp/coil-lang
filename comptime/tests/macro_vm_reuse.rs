//! #1008: function-style macro calls reuse one VM per expansion program
//! per thread. A call that panics or runs out of steps must not poison
//! the VM later calls run on, and many calls in a row must keep giving
//! the same output.

use std::path::{Path, PathBuf};

use compiler::Pipeline;

const PROVIDER: &str = r#"use macro::{Expr, Code, raw};

macro bump(Expr e) -> Code {
    if e.str() == "0" {
        panic "bump refuses 0";
    }
    if e.str() == "99" {
        let i = 0;
        while true {
            i = i + 1;
        }
    }
    return quote expr { ${e} + 1 };
}
"#;

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coil_macro_vm_reuse_{name}_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/gen.hy"), PROVIDER).unwrap();
    dir
}

fn compile(dir: &Path, file: &str, body: &str) -> Result<(), String> {
    let path = dir.join("src").join(file);
    std::fs::write(&path, format!("use gen::{{bump}};\n\nfn main() {{\n{body}}}\n")).unwrap();
    let mut pipeline = Pipeline::new();
    pipeline.bind_project_root(dir.to_path_buf(), vec!["src".into()]);
    pipeline
        .compile_src_from_file(path.to_str().unwrap())
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

#[test]
fn a_failing_macro_call_does_not_poison_later_calls() {
    comptime::install();
    let dir = project("poison");
    compile(&dir, "ok1.hy", "    let a: int = bump!(1);\n").expect("first call expands");
    assert!(compile(&dir, "panics.hy", "    let a: int = bump!(0);\n").is_err(), "a panicking macro fails the build");
    compile(&dir, "ok2.hy", "    let a: int = bump!(2);\n").expect("a call after a panic expands");
    assert!(compile(&dir, "spins.hy", "    let a: int = bump!(99);\n").is_err(), "a runaway macro fails the build");
    compile(&dir, "ok3.hy", "    let a: int = bump!(3);\n").expect("a call after a runaway expands");
}

#[test]
fn many_calls_on_one_vm_expand_alike() {
    comptime::install();
    let dir = project("many");
    let body: String = (100..400).map(|i| format!("    let a{i}: int = bump!({i});\n")).collect();
    compile(&dir, "many.hy", &body).expect("300 calls expand");
}
