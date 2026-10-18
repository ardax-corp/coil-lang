//! `coil compile` of a file with no `main` whose generic fns nothing
//! instantiates (a test-only file: `compile` leaves the test blocks out)
//! succeeds, at the default level and `-O0`. A shared generic body that
//! builds a `T`-dependent payload (`Option<(T, T)>`), a `T` aggregate, or a
//! derived instance method on a generic class has no direct MIR; such a body
//! aborted the compile with "fell back to the HIR stack lowering"
//! (coil-lang#1424).

use std::path::{Path, PathBuf};
use std::process::Command;

/// The exit status and stderr of `coil compile <mode> <file>`.
fn compile(bin: &str, file: &Path, out: &Path, mode: &[&str]) -> (bool, String) {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut cmd = Command::new(bin);
    cmd.arg("compile").args(mode).arg("--root").arg(ws);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    let res = cmd.arg(file).arg("-o").arg(out).output().expect("coil compile");
    (res.status.success(), String::from_utf8_lossy(&res.stderr).into_owned())
}

#[test]
fn uncalled_shared_generic_bodies_compile_without_main() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_uncalled_generic_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let snippets = [
        ("payload.hy", "fn pair<T>(T x) -> Option<(T, T)> {\n    return Option::Some((x, x));\n}\n"),
        ("aggregate.hy", "fn scale<T: Num>((T, T) v, T s) -> (T, T) {\n    return v * s;\n}\n"),
        (
            "derived.hy",
            "#[derive(Eq, Show)]\nclass Box<T> {\n    pub item: T,\n}\n\nfn same<T: Eq>(Box<T> a, Box<T> b) -> bool {\n    return a == b;\n}\n",
        ),
    ];
    let mut files: Vec<PathBuf> = Vec::new();
    for (name, src) in snippets {
        let path = dir.join(name);
        std::fs::write(&path, src).unwrap();
        files.push(path);
    }
    // The in-tree test-only files the issue lists.
    let ws = Path::new(env!("CARGO_MANIFEST_DIR"));
    for rel in [
        "tests/generics/generic_option_return_free.hy",
        "tests/generics/generic_class_returns.hy",
        "tests/generics/generic_nested_types.hy",
        "tests/generics/generic_elementwise.hy",
        "tests/traits/derive_generic_types.hy",
        "tests/traits/generic_instance_operators.hy",
    ] {
        files.push(ws.join(rel));
    }

    let out = dir.join("out.hyc");
    let mut failures = Vec::new();
    for file in &files {
        for mode in [&[][..], &["-O0"][..]] {
            let (ok, stderr) = compile(&bin, file, &out, mode);
            if !ok {
                failures.push(format!("{} {mode:?}:\n{stderr}", file.display()));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
