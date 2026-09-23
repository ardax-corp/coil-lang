// Matrix compares, bitwise ops, intersect, and diff.
// Compares and intersect/diff are byte masks of 0 and 1.
// Expected output: 10101001,10305008,10111101,01000010,00000001,2,221,1001

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn main() {
    let a = matrix([[1, 2, 3, 4], [5, 6, 7, 8]]);
    let b = matrix([[1, 0, 3, 9], [5, 1, 0, 8]]);
    let eq = a == b;
    let bits = a & b;
    let both = intersect(a, b);
    let only = diff(a, b);
    let hit = a == 8;
    let shifted = matrix([[1, 0], [0, 0]]) << 1;
    let q = matrix([[34 as byte, 0 as byte]]);
    let flipped = ~q;
    let f = matrix([[1.0, 2.0], [3.0, 4.0]]);
    let g = matrix([[0.0, 2.0], [9.0, 1.0]]);
    let gt = f > g;
    write_all(
        stdout(),
        to_bytes(format(
            "%i%i%i%i%i%i%i%i,",
            eq[0][0] as int,
            eq[0][1] as int,
            eq[0][2] as int,
            eq[0][3] as int,
            eq[1][0] as int,
            eq[1][1] as int,
            eq[1][2] as int,
            eq[1][3] as int,
        )),
    );
    write_all(
        stdout(),
        to_bytes(format(
            "%i%i%i%i%i%i%i%i,",
            bits[0][0],
            bits[0][1],
            bits[0][2],
            bits[0][3],
            bits[1][0],
            bits[1][1],
            bits[1][2],
            bits[1][3],
        )),
    );
    write_all(
        stdout(),
        to_bytes(format(
            "%i%i%i%i%i%i%i%i,",
            both[0][0] as int,
            both[0][1] as int,
            both[0][2] as int,
            both[0][3] as int,
            both[1][0] as int,
            both[1][1] as int,
            both[1][2] as int,
            both[1][3] as int,
        )),
    );
    write_all(
        stdout(),
        to_bytes(format(
            "%i%i%i%i%i%i%i%i,",
            only[0][0] as int,
            only[0][1] as int,
            only[0][2] as int,
            only[0][3] as int,
            only[1][0] as int,
            only[1][1] as int,
            only[1][2] as int,
            only[1][3] as int,
        )),
    );
    write_all(
        stdout(),
        to_bytes(format(
            "%i%i%i%i%i%i%i%i,",
            hit[0][0] as int,
            hit[0][1] as int,
            hit[0][2] as int,
            hit[0][3] as int,
            hit[1][0] as int,
            hit[1][1] as int,
            hit[1][2] as int,
            hit[1][3] as int,
        )),
    );
    write_all(
        stdout(),
        to_bytes(format("%i,%i,", shifted[0][0], flipped[0][0] as int)),
    );
    write_all(
        stdout(),
        to_bytes(format(
            "%i%i%i%i",
            gt[0][0] as int,
            gt[0][1] as int,
            gt[1][0] as int,
            gt[1][1] as int,
        )),
    );
}
