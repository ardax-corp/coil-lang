// Nested fixed arrays, element-wise tuple and array arithmetic, `dot`, a tuple-returning min/max and tuple destructuring.
// @output [[1, 2], [3, 4]] 10 (4, 6) [5, 7, 9] 11 (2, 3) 10\n
use io::sync::println;
use string::format;

fn transpose([[int; 2]; 2] m) -> [[int; 2]; 2] {
    return [[m[0][0], m[1][0]], [m[0][1], m[1][1]]];
}

fn sum_all([[int; 2]; 2] m) -> int {
    let t = 0;
    for row in m {
        for x in row {
            t += x;
        }
    }
    return t;
}

fn minmax(Vec<int> v) -> (int, int) {
    let lo = v[0];
    let hi = v[0];
    for x in v {
        if x < lo {
            lo = x;
        }
        if x > hi {
            hi = x;
        }
    }
    return (lo, hi);
}

fn swap((int, int) p) -> (int, int) {
    let (a, b) = p;
    return (b, a);
}

fn main() {
    let m = [[1, 3], [2, 4]];
    let t = transpose(m);
    let p = (1, 2) + (3, 4);
    let a = [1, 2, 3] + [4, 5, 6];
    let d = dot((1, 2), (3, 4));
    let (lo, hi) = minmax(Vec::from([5, 3, 9, 2, 7]));
    let sw = swap((3, 2));
    let _ = println(
        format("%v %i %v %v %i %v %i", t, sum_all(m), p, a, d, sw, lo * hi - 2 * lo * hi / 3 + 4),
    );
}
