//! Compile time of one function grows about linearly with its size
//! (coil-lang#1393): MIR emit used to rescan the whole function per block
//! (fused-compare and predecessor lookups) and the GC root pass per
//! allocation, so a body 8x longer took ~50x as long.

use std::time::{Duration, Instant};

use compiler::Pipeline;

/// `main` with `n` branches, each allocating.
fn long_main(n: usize) -> String {
    let mut src = String::from("fn main() {\n    let x = 1;\n    let s = \"\";\n");
    for i in 0..n {
        src.push_str(&format!("    if x == {i} {{\n        s = s + \"a\";\n    }}\n"));
    }
    src.push_str("}\n");
    src
}

/// Fastest of three compiles of `src`.
fn compile_time(src: &str) -> Duration {
    (0..3)
        .map(|_| {
            let mut pipeline = Pipeline::new();
            let start = Instant::now();
            pipeline.compile_src(src).expect("compiles");
            start.elapsed()
        })
        .min()
        .unwrap()
}

#[test]
fn one_long_function_compiles_in_about_linear_time() {
    let small = compile_time(&long_main(100));
    let large = compile_time(&long_main(800));
    // Linear is ~8x; the old quadratic passes made it ~50x.
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(1e-6);
    assert!(
        ratio < 24.0,
        "800 statements took {large:?}, 100 took {small:?} ({ratio:.1}x)"
    );
}
