//! #2206: `p = f(p)` moves `p` into the call, so updating a container
//! through a `V -> V` function copies nothing. Counted with
//! `COIL_COW_REPORT`, which reports the copies copy-on-write made.

use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

/// The copies `COIL_COW_REPORT` counts for `src` run as a program.
fn copies(name: &str, src: &str) -> u64 {
    let dir = std::env::temp_dir().join(format!("coil_cow_call_share_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.coil");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(coil_bin()).env("COIL_COW_REPORT", "1").arg(&file).output().expect("spawn coil");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{name} failed: {stderr}");
    let line = stderr.lines().find(|l| l.starts_with("cow-report:")).unwrap_or_else(|| panic!("{name}: no cow-report in {stderr}"));
    line.trim_start_matches("cow-report: ").split_whitespace().next().unwrap().parse().unwrap()
}

const N: u64 = 2000;

#[test]
fn reassigning_a_local_from_a_call_copies_nothing() {
    let src = format!(
        "
fn add(Vec<string> vs) -> Vec<string> {{
    vs.push(\"x\");
    return vs;
}}
fn main() {{
    let vs: Vec<string> = Vec::new();
    let i = 0;
    while i < {N} {{
        vs = add(vs);
        i = i + 1;
    }}
    if vs.len() != {N} {{ panic \"wrong length\"; }}
}}
"
    );
    let n = copies("local", &src);
    assert!(n < 4, "{n} copies for {N} calls");
}

#[test]
fn reassigning_through_a_function_value_copies_nothing() {
    let src = format!(
        "
fn grow(Vec<int> vs, Vec<int> -> Vec<int> f) -> Vec<int> {{
    let i = 0;
    while i < {N} {{
        vs = f(vs);
        i = i + 1;
    }}
    return vs;
}}
fn push_one(Vec<int> v) -> Vec<int> {{
    v.push(1);
    return v;
}}
fn main() {{
    let vs = grow(Vec::new(), push_one);
    if vs.len() != {N} {{ panic \"wrong length\"; }}
}}
"
    );
    let n = copies("lambda", &src);
    assert!(n < 4, "{n} copies for {N} calls");
}
