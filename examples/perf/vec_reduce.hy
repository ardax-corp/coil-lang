// Counted reduce and map loops over 8192 elements: a sum whose function
// is only the loop, the same sum after an early return, a dot product and
// a scaled copy. Prints ns per call; the compiler runs each loop 8 lanes
// at a time in one op (`mir/vec_loop.rs`).
use clock::mono_nanos;
use io::{stdout, write};
use string::{format, to_bytes};

fn say(string s) {
    let _ = write(stdout(), to_bytes(s));
}

fn sum(Vec<int> xs, int from) -> int {
    let s = 0;
    let i = from;
    while i < xs.len() {
        s = s + xs[i];
        i = i + 1;
    }
    return s;
}

fn sum_guarded(Vec<int> xs, int from) -> int {
    if xs.len() == 0 {
        return -1;
    }
    let s = 0;
    let i = from;
    while i < xs.len() {
        s = s + xs[i];
        i = i + 1;
    }
    return s;
}

fn dot(Vec<float> a, Vec<float> b, int from) -> float {
    let s = 0.0;
    let i = from;
    while i < a.len() {
        s = s + a[i] * b[i];
        i = i + 1;
    }
    return s;
}

fn scale(Vec<float> out, Vec<float> a, float k, int from) -> int {
    if a.len() > out.len() {
        return -1;
    }
    let i = from;
    while i < a.len() {
        out[i] = a[i] * k + 1.0;
        i = i + 1;
    }
    return i;
}

fn main() {
    let n = 8192;
    let xs: Vec<int> = Vec::new();
    let fs: Vec<float> = Vec::new();
    let out: Vec<float> = Vec::new();
    let j = 0;
    while j < n {
        xs.push(j % 1000);
        fs.push((j % 100) as float);
        out.push(0.0);
        j = j + 1;
    }
    let reps = 2000;
    let acc = 0;
    let facc = 0.0;
    let t0 = mono_nanos();
    let k = 0;
    while k < reps {
        acc = acc + sum(xs, k % 3);
        k = k + 1;
    }
    let t1 = mono_nanos();
    k = 0;
    while k < reps {
        acc = acc + sum_guarded(xs, k % 3);
        k = k + 1;
    }
    let t2 = mono_nanos();
    k = 0;
    while k < reps {
        facc = facc + dot(fs, fs, k % 3);
        k = k + 1;
    }
    let t3 = mono_nanos();
    k = 0;
    while k < reps {
        acc = acc + scale(out, fs, 0.5, k % 3);
        k = k + 1;
    }
    let t4 = mono_nanos();
    say(format("acc=%i facc=%f out=%f\n", acc, facc, out[777]));
    say(format("sum          %i ns\n", (t1 - t0) / reps));
    say(format("sum_guarded  %i ns\n", (t2 - t1) / reps));
    say(format("dot          %i ns\n", (t3 - t2) / reps));
    say(format("scale        %i ns\n", (t4 - t3) / reps));
}
