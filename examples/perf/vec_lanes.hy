// Map loops over 8192 elements that need lane casts, selects or pushes. Prints
// ns per call;
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

// `if x < 0.0 { x = -x; }`: a select through a join.
fn abs_map(Vec<float> out, Vec<float> a, int from) {
    let i = from;
    while i < a.len() {
        let x = a[i];
        if x < 0.0 {
            x = 0.0 - x;
        }
        out[i] = x;
        i = i + 1;
    }
}

// A store in each arm.
fn clamp_map(Vec<float> out, Vec<float> a, float hi, int from) {
    let i = from;
    while i < a.len() {
        let x = a[i];
        if x > hi {
            out[i] = hi;
        } else {
            out[i] = x * 2.0;
        }
        i = i + 1;
    }
}

// ASCII lower case in place of a copy: a ranged `int` add in an arm.
fn lower_bytes(Vec<byte> out, Vec<byte> b, int from) {
    let i = from;
    while i < b.len() {
        let c = b[i];
        if c >= "A" && c <= "Z" {
            out[i] = ((c as int) + 32) as byte;
        } else {
            out[i] = c;
        }
        i = i + 1;
    }
}

// `out.push(f(a[i]))` onto a fresh array: a `VLoop` append.
fn mul_push(Vec<int> a, int from) -> Vec<int> {
    let out: Vec<int> = Vec::new();
    let i = from;
    while i < a.len() {
        out.push(a[i] * 3 + 1);
        i = i + 1;
    }
    return out;
}

// `text::shift_ascii`'s shape: a push in each arm.
fn shift_bytes(Vec<byte> b, int delta, int from) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = from;
    while i < b.len() {
        let c = b[i];
        if c >= "A" && c <= "Z" {
            out.push(((c as int) + delta) as byte);
        } else {
            out.push(c);
        }
        i = i + 1;
    }
    return out;
}

fn main() {
    let n = 8192;
    let xs: Vec<int> = Vec::new();
    let fs: Vec<float> = Vec::new();
    let out: Vec<float> = Vec::new();
    let text: Vec<byte> = Vec::new();
    let low: Vec<byte> = Vec::new();
    let j = 0;
    while j < n {
        text.push((48 + (j * 7) % 75) as byte);
        low.push(0 as byte);
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
    k = 0;
    while k < reps {
        abs_map(out, fs, k % 3);
        k = k + 1;
    }
    let t3 = mono_nanos();
    k = 0;
    while k < reps {
        clamp_map(out, fs, 20.0, k % 3);
        k = k + 1;
    }
    let t4 = mono_nanos();
    k = 0;
    while k < reps {
        lower_bytes(low, text, k % 3);
        k = k + 1;
    }
    let t5 = mono_nanos();
    let pushed = 0;
    k = 0;
    while k < reps {
        pushed = pushed + mul_push(xs, k % 3).len();
        k = k + 1;
    }
    let t6 = mono_nanos();
    k = 0;
    while k < reps {
        pushed = pushed + shift_bytes(text, 32, k % 3).len();
        k = k + 1;
    }
    let t7 = mono_nanos();
    say(format("trunc_map %i ns\n", (t1 - t0) / reps));
    say(format("halves    %i ns\n", (t2 - t1) / reps));
    say(format("abs_map   %i ns\n", (t3 - t2) / reps));
    say(format("clamp_map %i ns\n", (t4 - t3) / reps));
    say(format("lower     %i ns\n", (t5 - t4) / reps));
    say(format("mul_push  %i ns\n", (t6 - t5) / reps));
    say(format("shift     %i ns\n", (t7 - t6) / reps));
    say(format("check %f %i %i\n", out[17] + out[n - 1], low[n - 1] as int, pushed));
}
