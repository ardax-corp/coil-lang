// A column-order nest over one flat array (`mir::interchange_loops`) runs
// row by row when a guard proves every index distinct and in bounds; the
// results match the source order. When the guard fails, the source order
// runs, so a panic leaves the stores the source made before it.
// @output map 1.0 15.0 29.0 15.0\nalias 3.0 29.0\nshort 0.0 19.0 15.0\nempty 0.0\n
use io::sync::println;
use string::format;
use thread::{join, spawn};

fn cols(Vec<float> a, Vec<float> b, int n, int m) {
    let j = 0;
    while j < m {
        let i = 0;
        while i < n {
            b[i * m + j] = a[i * m + j] * 2.0 + 1.0;
            i = i + 1;
        }
        j = j + 1;
    }
}

fn ramp(int len) -> Vec<float> {
    let v: Vec<float> = Vec::new();
    let k = 0;
    while k < len {
        v.push(k as float);
        k = k + 1;
    }
    return v;
}

fn zeros(int len) -> Vec<float> {
    let v: Vec<float> = Vec::new();
    let k = 0;
    while k < len {
        v.push(0.0);
        k = k + 1;
    }
    return v;
}

fn total(Vec<float> v) -> float {
    let s = 0.0;
    for x in v {
        s = s + x;
    }
    return s;
}

fn run_short() -> int {
    // Twelve cells over ten: the source order stores column 0, then
    // cells 1, 4 and 7 of column 1, and panics at 10.
    let a = ramp(10);
    let b = zeros(10);
    defer use (b) {
        let _ = println(format("short %v %v %v", b[2], b[9], b[7]));
    }
    cols(a, b, 4, 3);
    return 0;
}

fn main() {
    let a = ramp(15);
    let b = zeros(15);
    cols(a, b, 5, 3);
    let _ = println(format("map %v %v %v %v", b[0], b[7], b[14], total(b) / 15.0));
    // Reading and writing one array.
    let c = ramp(15);
    cols(c, c, 5, 3);
    let _ = println(format("alias %v %v", c[1], c[14]));
    let t = match spawn(run_short) {
        Result::Ok(t) => t,
        Result::Err(_) => panic "spawn failed",
    };
    let _ = join(t);
    let e = zeros(4);
    cols(ramp(4), e, 0, 2);
    let _ = println(format("empty %v", total(e)));
}
