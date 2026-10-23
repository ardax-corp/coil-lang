//! `--coverage` keeps calls the default `-O` would compute at compile time
//! (constant arguments, #1856): their bodies run, so their lines count and
//! `coil mutate` sees them covered. Runs the binary, which links the macro
//! host that call folding needs.

use std::process::Command;

#[test]
fn coverage_counts_a_call_folding_would_compute() {
    let root = std::env::temp_dir().join(format!(
        "coil_test_coverage_call_fold_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(
        root.join("src/mathx.hy"),
        "fn clamp(int x, int lo, int hi) -> int {\n    if x < lo {\n        return lo;\n    }\n    if x > hi {\n        return hi;\n    }\n    return x;\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("tests/t.hy"),
        "use mathx::clamp;\n\ntest(\"clamps low\") {\n    let low = clamp(-5, 0, 10);\n    if low != 0 { raise \"assertion failed\"; }\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_coil-test"))
        .current_dir(&root)
        .args(["--no-shuffle", "-j", "1", "--coverage", "--root", "src", "tests"])
        .env("COIL_PROGRESS", "0")
        .env_remove("COIL_CALL_FOLD")
        .output()
        .expect("spawn coil-test");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    let lcov = std::fs::read_to_string(root.join("target/coverage/lcov.info")).expect("lcov written");
    let hits = |line: u32| {
        lcov.lines()
            .find_map(|l| l.strip_prefix(&format!("DA:{line},")))
            .and_then(|n| n.parse::<u64>().ok())
    };
    assert!(hits(2).is_some_and(|n| n > 0), "the `if` line is covered: {lcov}");
    assert!(hits(3).is_some_and(|n| n > 0), "the `return lo` line is covered: {lcov}");
    let _ = std::fs::remove_dir_all(&root);
}
