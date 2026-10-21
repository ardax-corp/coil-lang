//! The comptime result cache (`--comptime-cache`, `--comptime-offline`,
//! `--comptime-refresh`) across separate builds: a const or macro that read
//! the environment keeps its cached result until refreshed, one that read a
//! file is computed again when the file changes, and offline builds use only
//! cached results.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Project {
    bin: String,
    dir: PathBuf,
}

impl Project {
    fn new(name: &str) -> Self {
        let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
        let dir = std::env::temp_dir().join(format!("coil_comptime_cache_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Project { bin, dir }
    }

    fn write(&self, name: &str, text: &str) {
        let path = self.dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Build and run `main.hy` with `flags` and `CT_VALUE=value`: success,
    /// stdout, stderr.
    fn run(&self, value: &str, flags: &[&str]) -> (bool, String, String) {
        let mut cmd = Command::new(&self.bin);
        cmd.current_dir(&self.dir).env("CT_VALUE", value);
        cmd.args(["--root", ".", "--comptime-cache", "cache", "--comptime-env", "CT_VALUE"]);
        cmd.args(flags);
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        let out = cmd.arg(Path::new("main.hy")).output().expect("coil run");
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        (out.status.success(), text(&out.stdout), text(&out.stderr))
    }
}

const PRINT: &str = "use io::{stdout, write};\nuse string::to_bytes;\n";

#[test]
fn an_environment_read_is_kept_until_refreshed() {
    let p = Project::new("env");
    p.write(
        "main.hy",
        &format!("use env::var;\n{PRINT}\nconst V = var(\"CT_VALUE\") ?? \"none\";\n\nfn main() {{\n    write(stdout(), to_bytes(V));\n}}\n"),
    );
    assert_eq!(p.run("one", &[]).1, "one");
    let (ok, out, _) = p.run("two", &[]);
    assert!(ok);
    assert_eq!(out, "one", "the cached value");
    let (ok, out, err) = p.run("two", &["--comptime-refresh"]);
    assert!(ok, "{err}");
    assert_eq!(out, "two");
    assert!(err.contains("E0135") && err.contains("has a new value"), "{err}");
    let (_, out, err) = p.run("three", &["--comptime-offline"]);
    assert_eq!(out, "two", "offline uses the cached value: {err}");

    std::fs::remove_dir_all(p.dir.join("cache")).unwrap();
    let (ok, _, err) = p.run("three", &["--comptime-offline"]);
    assert!(!ok);
    assert!(err.contains("--comptime-offline") && err.contains("`CT_VALUE`"), "{err}");
}

#[test]
fn a_changed_file_is_read_again() {
    let p = Project::new("file");
    p.write(
        "main.hy",
        &format!(
            "use io::file::read_text;\nuse env::var;\n{PRINT}\nconst V = (read_text(\"data/v.txt\") ?? \"\") + (var(\"CT_VALUE\") ?? \"\");\n\nfn main() {{\n    write(stdout(), to_bytes(V));\n}}\n"
        ),
    );
    p.write("data/v.txt", "a");
    let read = ["--comptime-read", "data"];
    assert_eq!(p.run("1", &read).1, "a1");
    assert_eq!(p.run("2", &read).1, "a1", "unchanged file: cached");
    p.write("data/v.txt", "b");
    assert_eq!(p.run("2", &read).1, "b2", "changed file: computed again");
    // Offline, a miss may not read the environment.
    p.write("data/v.txt", "c");
    let (ok, _, err) = p.run("3", &["--comptime-read", "data", "--comptime-offline"]);
    assert!(!ok, "{err}");
}

#[test]
fn a_macro_result_is_cached() {
    let p = Project::new("macro");
    p.write(
        "gen.hy",
        "use macro::{Expr, Code, lit};\nuse env::var;\n\nmacro env_text(Expr name) -> Code {\n    return lit(var(name.src()) ?? \"\");\n}\n",
    );
    p.write(
        "main.hy",
        &format!("use gen::env_text;\n{PRINT}\nfn main() {{\n    write(stdout(), to_bytes(env_text!(CT_VALUE)));\n}}\n"),
    );
    assert_eq!(p.run("one", &[]).1, "one");
    assert_eq!(p.run("two", &[]).1, "one", "the cached output");
    let (_, out, err) = p.run("two", &["--comptime-refresh"]);
    assert_eq!(out, "two");
    assert!(err.contains("E0135") && err.contains("has new output"), "{err}");
    assert_eq!(p.run("three", &["--comptime-offline"]).1, "two");
}

#[test]
fn a_cached_class_value_survives_new_types() {
    let p = Project::new("types");
    p.write("m/a.hy", "class A1 {\n    pub x: int,\n}\n");
    p.write(
        "m/b.hy",
        "use env::var;\n\nclass P {\n    pub name: string,\n    pub n: int,\n}\n\nconst X = new P(var(\"CT_VALUE\") ?? \"none\", 7);\n",
    );
    p.write(
        "main.hy",
        &format!(
            "use m::a::A1;\nuse m::b::X;\nuse string::format;\n{PRINT}\nfn main() {{\n    let a = new A1(1);\n    write(stdout(), to_bytes(format(\"%s %i\", X.name, X.n + a.x)));\n}}\n"
        ),
    );
    assert_eq!(p.run("one", &[]).1, "one 8");
    // A class declared earlier moves `P`'s type id; the cached value follows.
    p.write("m/a.hy", "class A0 {\n    pub y: int,\n}\n\nclass A1 {\n    pub x: int,\n}\n");
    let (ok, out, err) = p.run("two", &[]);
    assert!(ok, "{err}");
    assert_eq!(out, "one 8");
}
