//! Macros and consts run on the MIR interpreter, and a program read back
//! from the disk cache does too: there is no other engine for them to run on.

use std::path::{Path, PathBuf};
use std::process::Command;

use compiler::Pipeline;

/// Set in a child run: the project to compile.
const CHILD: &str = "COIL_MACRO_ENGINE_PROJECT";

const PROVIDER: &str = r#"use macro::{Expr, Code};

macro bump(Expr e) -> Code {
    return quote expr { ${e} + 1 };
}
"#;

const USER: &str = r#"use gen::bump;

fn square(int n) -> int {
    return n * n;
}

const SIDE: int = square(12);

fn main() {
    let _ = bump!(SIDE);
}
"#;

fn project(root: &Path) -> PathBuf {
    let dir = root.join("project");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/gen.coil"), PROVIDER).unwrap();
    std::fs::write(dir.join("src/main.coil"), USER).unwrap();
    dir
}

/// In a child process: compile the project.
fn compile(dir: &Path) {
    comptime::install();
    let mut pipeline = Pipeline::new();
    pipeline.bind_project_root(dir.to_path_buf(), vec![PathBuf::from("src")]);
    let file = dir.join("src/main.coil");
    pipeline.compile_src_from_file(file.to_str().unwrap()).expect("the program compiles");
}

/// Each run is a process of its own, so the second one reads the expansion
/// program back from the disk cache the first one wrote: the MIR has to
/// survive that.
#[test]
fn macros_and_consts_run_on_the_interpreter() {
    if let Some(dir) = std::env::var_os(CHILD) {
        compile(Path::new(&dir));
        return;
    }
    let root = std::env::temp_dir().join(format!("coil_macro_engine_{}", std::process::id()));
    let dir = project(&root);
    let run = || {
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "macros_and_consts_run_on_the_interpreter", "--nocapture"])
            .env(CHILD, &dir)
            .env("XDG_CACHE_HOME", root.join("cache"))
            .status()
            .unwrap();
        assert!(status.success(), "the child's checks failed");
    };
    run();
    let programs = root.join("cache/coil/macro-programs");
    let cached = std::fs::read_dir(&programs).map_or(0, Iterator::count);
    assert!(cached > 0, "the first run cached its expansion program in {}", programs.display());
    run();
    let _ = std::fs::remove_dir_all(&root);
}
