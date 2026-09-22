// Iterative sibling of tak.hy. Same tak(18, 12, 6) = 7.
// An explicit stack walks the same call tree. There is no VM recursion
// and no stride-1 vector loop: each step still waits on three children.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn tak_iter(int x0, int y0, int z0) -> int {
    let xs: Vec<int> = Vec::new();
    let ys: Vec<int> = Vec::new();
    let zs: Vec<int> = Vec::new();
    let phase: Vec<int> = Vec::new();
    let aa: Vec<int> = Vec::new();
    let bb: Vec<int> = Vec::new();
    xs.push(x0);
    ys.push(y0);
    zs.push(z0);
    phase.push(0);
    aa.push(0);
    bb.push(0);
    let result = 0;
    while xs.len() > 0 {
        let n = xs.len() - 1;
        let x = xs[n];
        let y = ys[n];
        let z = zs[n];
        let p = phase[n];
        if p == 0 {
            if y >= x {
                result = z;
                let _ = xs.pop();
                let _ = ys.pop();
                let _ = zs.pop();
                let _ = phase.pop();
                let _ = aa.pop();
                let _ = bb.pop();
            } else {
                phase[n] = 1;
                xs.push(x - 1);
                ys.push(y);
                zs.push(z);
                phase.push(0);
                aa.push(0);
                bb.push(0);
            }
        } else if p == 1 {
            aa[n] = result;
            phase[n] = 2;
            xs.push(y - 1);
            ys.push(z);
            zs.push(x);
            phase.push(0);
            aa.push(0);
            bb.push(0);
        } else if p == 2 {
            bb[n] = result;
            phase[n] = 3;
            xs.push(z - 1);
            ys.push(x);
            zs.push(y);
            phase.push(0);
            aa.push(0);
            bb.push(0);
        } else if p == 3 {
            phase[n] = 4;
            xs.push(aa[n]);
            ys.push(bb[n]);
            zs.push(result);
            phase.push(0);
            aa.push(0);
            bb.push(0);
        } else {
            let _ = xs.pop();
            let _ = ys.pop();
            let _ = zs.pop();
            let _ = phase.pop();
            let _ = aa.pop();
            let _ = bb.pop();
        }
    }
    return result;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", tak_iter(18, 12, 6))));
}
