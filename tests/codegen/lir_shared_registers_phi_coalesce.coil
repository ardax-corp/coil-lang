// A body with more values than frame words shares LIR registers between
// values never live at once. The phi coalescing that follows must not alias
// two values onto a register another live value holds: two scalar-replaced
// 20-element arrays plus indexed loops overwrote the loop bound and read
// out of bounds.
use tests::assert;

fn convolve_for(float x0, float x1, int nx, int j) -> float {
    let xs = [x0, x1];
    let f = [
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0,
    ];
    let q = [
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0,
    ];
    let jx = nx - 1;
    for i in 0..jx + 5 {
        if j < 0 {
            f[i] = 1.0;
        }
    }
    for i in 0..5 {
        let fw = 0.0;
        for jj in 0..jx + 1 {
            fw = fw + xs[jj] * f[jx + i - jj];
        }
        q[i] = fw;
    }
    return q[0] + q[4];
}

fn convolve_while(float x0, float x1, float x2, int nx, int e0) -> float {
    let table = [0xA2F983, 0x6E4E44, 0x1529FC, 0x2757D1, 0xF534DD, 0xC0DB62, 0x95993C, 0x439041];
    let xs = [x0, x1, x2];
    let f = [
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0,
    ];
    let q = [
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0,
    ];
    let jk = 4;
    let jx = nx - 1;
    let jv = (e0 - 3) / 24;
    if jv < 0 {}
    let j = jv - jx;
    let i = 0;
    while i <= jx + jk {
        if j < 0 {
            f[i] = 0.0;
            f[i] = table[j + 8] as float;
        }
        i = i + 1;
    }
    i = 0;
    while i <= jk {
        let fw = 0.0;
        let jj = 0;
        while jj <= jx {
            fw = fw + xs[jj] * f[jx + i - jj];
            jj = jj + 1;
        }
        q[i] = fw;
        i = i + 1;
    }
    return q[0] + q[4];
}

test("for loops over two scalar-replaced arrays keep their bounds") {
    assert!(convolve_for(3.0, 5.0, 2, 0) == 0.0);
    assert!(convolve_for(3.0, 5.0, 2, -1) == 16.0);
}

test("while loops over two scalar-replaced arrays keep their bounds") {
    assert!(convolve_while(3.0, 5.0, 7.0, 2, 30) == 0.0);
    assert!(convolve_while(1.0, 1.0, 1.0, 2, 3) == 4.0 * 0x439041 as float);
}
