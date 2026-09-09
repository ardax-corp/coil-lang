// CPU: counted saxpy store `y[i] = a * x[i] + y[i]` — S5b V1 conservative FMA.
// Mul-then-add (two IEEE roundings), same as scalar Coil `*`.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn axpy(float a, Vec<float> x, Vec<float> y) -> float {
    let i = 0;
    while i < len(x) {
        y[i] = a * x[i] + y[i];
        i = i + 1;
    }
    return y[0];
}

fn checksum(Vec<float> y) -> float {
    let s = 0.0;
    let i = 0;
    while i < len(y) {
        s = s + y[i];
        i = i + 1;
    }
    return s;
}

fn main() {
    let n = 1 << 12;
    let x: Vec<float> = Vec::with_capacity(n);
    let y: Vec<float> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        x.push(i as float);
        y.push(1.0);
        i = i + 1;
    }
    let round = 0;
    while round < 64 {
        let _ = axpy(2.0, x, y);
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", checksum(y) as int)));
}
