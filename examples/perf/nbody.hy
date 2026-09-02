// Canary: float dense loop (spectral-norm). N-body literals/stability were
// painful under the float lexer (`0.01` / `1.03` do not parse); this is the
// same numeric-beyond-numeric canary. A later float-loop cut should drop
// VM-only time vs this baseline.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn eval_a(int i, int j) -> float {
    let ij = i + j;
    let t = (ij * (ij + 1)) / 2 + i + 1;
    return 1.0 / (t as float);
}

fn times_a(Vec<float> v, Vec<float> out) {
    let n = len(v);
    let i = 0;
    while i < n {
        let s = 0.0;
        let j = 0;
        while j < n {
            s = s + eval_a(i, j) * v[j];
            j = j + 1;
        }
        out[i] = s;
        i = i + 1;
    }
}

fn times_at(Vec<float> v, Vec<float> out) {
    let n = len(v);
    let i = 0;
    while i < n {
        let s = 0.0;
        let j = 0;
        while j < n {
            s = s + eval_a(j, i) * v[j];
            j = j + 1;
        }
        out[i] = s;
        i = i + 1;
    }
}

fn at_a_u(Vec<float> u, Vec<float> v, Vec<float> tmp) {
    times_a(u, tmp);
    times_at(tmp, v);
}

fn main() {
    let n = 250;
    let u: Vec<float> = Vec::with_capacity(n);
    let v: Vec<float> = Vec::with_capacity(n);
    let tmp: Vec<float> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        u.push(1.0);
        v.push(0.0);
        tmp.push(0.0);
        i = i + 1;
    }
    let round = 0;
    while round < 10 {
        at_a_u(u, v, tmp);
        at_a_u(v, u, tmp);
        round = round + 1;
    }
    let vbv = 0.0;
    let vv = 0.0;
    i = 0;
    while i < n {
        vbv = vbv + u[i] * v[i];
        vv = vv + v[i] * v[i];
        i = i + 1;
    }
    let result = sqrt(vbv / vv);
    write_all(stdout(), to_bytes(format("%i", (result * 1000000000.0) as int)));
}
