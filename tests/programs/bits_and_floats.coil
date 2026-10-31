// Bit tricks (popcount, power-of-two test, shifts, hex literal, `~`) next to float math (stdlib `sqrt`, `floor`, `ceil`; prelude `pow`) and casts.
// @output 3 1 8 255 -1 true 3.0 2.0 1.4142135623730951 5.0 0.5 1 2 true\n
use io::sync::println;
use string::format;
use math::{ceil, floor, sqrt};

fn popcount(int n) -> int {
    let c = 0;
    while n != 0 {
        c += n & 1;
        n = n >> 1;
    }
    return c;
}

fn is_pow2(int n) -> bool {
    return n > 0 && (n & (n - 1)) == 0;
}

fn hypot(float a, float b) -> float {
    return sqrt(a * a + b * b);
}

fn clamp(float x, float lo, float hi) -> float {
    if x < lo {
        return lo;
    }
    if x > hi {
        return hi;
    }
    return x;
}

fn main() {
    let _ = println(
        format(
            "%i %i %i %i %i %z %f %f %f %f %f %i %i %z",
            popcount(7),
            5 ^ 4,
            1 << 3,
            0xff,
            ~0,
            is_pow2(64),
            floor(3.7),
            ceil(1.2),
            sqrt(2.0),
            hypot(3.0, 4.0),
            clamp(0.5, 0.0, 1.0),
            7 % 3,
            pow(2.0, 1.0) as int,
            0.1 + 0.2 != 0.3,
        ),
    );
}
