// Map loops over 8192 elements that need lane casts. Prints ns per call;
// the compiler runs each loop 8 lanes at a time in one op
// (`mir/vec_loop.rs`, `docs/internals/simd-lanes.md`).
use clock::mono_nanos;
use io::{stdout, write};
use string::{format, to_bytes};

fn say(string s) {
    let _ = write(stdout(), to_bytes(s));
}

// `(a[i] as int) as float`: truncate toward zero.
fn trunc_map(Vec<float> out, Vec<float> a, int from) {
    let i = from;
    while i < a.len() {
        out[i] = (a[i] as int) as float;
        i = i + 1;
    }
}

// Int lanes scaled as floats.
fn halves(Vec<float> out, Vec<int> a, int from) {
    let i = from;
    while i < a.len() {
        out[i] = (a[i] as float) * 0.5;
        i = i + 1;
    }
}

fn main() {
    let n = 8192;
    let xs: Vec<int> = Vec::new();
    let fs: Vec<float> = Vec::new();
    let out: Vec<float> = Vec::new();
    let j = 0;
    while j < n {
        xs.push(j % 1000 - 500);
        fs.push((j % 100) as float * 1.37 - 60.0);
        out.push(0.0);
        j = j + 1;
    }
    let reps = 2000;
    let t0 = mono_nanos();
    let k = 0;
    while k < reps {
        trunc_map(out, fs, k % 3);
        k = k + 1;
    }
    let t1 = mono_nanos();
    k = 0;
    while k < reps {
        halves(out, xs, k % 3);
        k = k + 1;
    }
    let t2 = mono_nanos();
    say(format("trunc_map %i ns\n", (t1 - t0) / reps));
    say(format("halves    %i ns\n", (t2 - t1) / reps));
    say(format("check %f\n", out[17] + out[n - 1]));
}
