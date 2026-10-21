// Element-wise unary `-`, float `/` and scalar-left `-` on 8-wide arrays,
// float tuple negation, and matrix `-` / unary `-`, checked as printed.
use tests::assert;
use string::format;

test("8-wide int negate, float divide and scalar-left subtract") {
    let a = [1, 2, 3, 4, 5, 6, 7, 8];
    let n = -a;
    let f = [2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0];
    let d = f / [2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0];
    let s = 10 - a;
    let out = format("%i%i,", n[0], n[7]) + format("%f%f,", d[0], d[7]) +
              format("%i%i", s[0], s[7]);
    assert!(out == "-1-8,1.08.0,92", out);
}

test("a float tuple negates as floats") {
    let d = -(1.5, 2.0);
    let out = format("%f,%f", d[0], d[1]);
    assert!(out == "-1.5,-2.0", out);
}

test("matrix subtract and negate") {
    let a = matrix([[5, 7], [9, 11]]);
    let b = matrix([[1, 2], [3, 4]]);
    let s = a - b;
    let n = -b;
    let out = format("%i,%i,%i,%i,", s[0][0], s[0][1], s[1][0], s[1][1]) +
              format("%i,%i,%i,%i", n[0][0], n[0][1], n[1][0], n[1][1]);
    assert!(out == "4,5,6,7,-1,-2,-3,-4", out);
}
