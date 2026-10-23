// Hit bench for #1434: MIR constant folding on LIR bodies. A body whose
// only arithmetic is bitwise ops and shifts takes LIR (no dense attempt),
// and its constants only meet once `mask` inlines.
//
// Prints with `write_all` rather than `print` (#1879): perf_metrics reads
// `check`'s own body, and under `print` the HIR inliner folds `check` into
// `main`.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn mask(int a) -> int {
    return a & 4294967295;
}

fn mix(int x) -> int {
    let m = mask(-1);
    let a = (x ^ (m >> 16)) & m;
    let b = (a << 3) ^ ((m >> 8) | (m << 5));
    let c = (b >> 7) ^ ((m >> 20) & b);
    let d = (c & (m >> 1)) | ((m >> 28) << 2);
    let e = (d ^ ((m >> 12) & (m << 4))) & (m >> 3);
    let f = (e << 2) ^ (e >> 5) ^ ((m >> 24) | (m >> 30));
    let g = (f & (m >> 2)) ^ ((m << 7) & (m >> 9));
    let h = (g ^ (a & (m >> 17))) & m;
    let j = (h >> 11) ^ ((m >> 5) & (h << 1));
    let k = (j | (m >> 29)) ^ ((m << 9) & (m >> 6));
    let l = (k & (m >> 4)) ^ ((m >> 14) | (m << 2));
    let n = (l << 4) ^ (l >> 9) ^ ((m >> 21) & (m << 3));
    let o = (n ^ ((m >> 13) | (m >> 27))) & (m >> 2);
    let p = (o >> 1) ^ ((m << 6) & (m >> 10));
    let q = (p & (m >> 3)) | ((m >> 26) << 1);
    return (q ^ (h & (m >> 19))) & m;
}

fn check() -> int {
    let m = mask(-1);
    return m >> 16;
}

fn main() {
    let i = 0;
    let acc = 0;
    while i < 2000000 {
        acc = (acc + mix(i)) & 1099511627775;
        i = i + 1;
    }
    let _ = write_all(stdout(), to_bytes(format("%i %i", check(), acc)));
}
