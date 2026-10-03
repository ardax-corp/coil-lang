// Regression: coil-stdlib num.hy (verbatim, minus coil-time). The float
// overloads ahead of lcm once made its `a * b` compile to MULF, so
// lcm(4, 6) returned 0.
use prelude::math::{pow as float_pow};

// Circle constant π (IEEE f64). `///` would bind to a following `fn`.
static const PI = 3.141592653589793;
// Natural logarithm base e (IEEE f64).
static const E = 2.718281828459045;
// Full turn τ = 2π (IEEE f64).
static const TAU = 6.283185307179586;

/// Absolute value of an integer.
fn abs(int x) -> int {
    if x < 0 {
        return 0 - x;
    }
    return x;
}

/// Absolute value of a float (preserves signed zero).
fn abs(float x) -> float {
    if x > 0.0 {
        return x;
    }
    if x == 0.0 {
        return 0.0;
    }
    return 0.0 - x;
}

/// Smaller of two `Ord` values.
fn min<T: Ord>(T a, T b) -> T {
    if a < b {
        return a;
    }
    return b;
}

/// Larger of two `Ord` values.
fn max<T: Ord>(T a, T b) -> T {
    if a > b {
        return a;
    }
    return b;
}

/// Nearest int as float; halves round away from zero (via trunc bias).
fn round(float x) -> float {
    if x >= 0.0 {
        return floor(x + 0.5);
    }
    return ceil(x - 0.5);
}

/// Clamp `x` into the inclusive range `[lo, hi]`.
fn clamp<T: Ord>(T x, T lo, T hi) -> T {
    return min(max(x, lo), hi);
}

/// Float power via libm (`base ** exp` IEEE semantics).
fn pow(float a, float b) -> float {
    return float_pow(a, b);
}

/// Integer power `base ** exp` for `exp >= 0`; `exp == 0` → `1`; `exp < 0` → `0`.
fn pow(int base, int exp) -> int {
    if exp < 0 {
        return 0;
    }
    if exp == 0 {
        return 1;
    }
    let r = 1;
    let i = 0;
    while i < exp {
        r = r * base;
        i = i + 1;
    }
    return r;
}

/// Sign of an integer (`-1`, `0`, or `1`).
fn signum(int x) -> int {
    if x < 0 {
        return 0 - 1;
    }
    if x > 0 {
        return 1;
    }
    return 0;
}

/// Sign of a float (`-1.0`, `0.0`, or `1.0`; preserves signed zero).
fn signum(float x) -> float {
    if x > 0.0 {
        return 1.0;
    }
    if x < 0.0 {
        return 0.0 - 1.0;
    }
    return 0.0;
}

/// Greatest common divisor for non-negative integers.
fn gcd(int a, int b) -> int {
    if b == 0 {
        let x = a;
        if x < 0 {
            return 0 - x;
        }
        return x;
    }
    let r = a % b;
    return gcd(b, r);
}

/// Least common multiple for non-negative integers (`0` if either operand is `0`).
fn lcm(int a, int b) -> int {
    if a == 0 {
        return 0;
    }
    if b == 0 {
        return 0;
    }
    let p = a * b;
    if p < 0 {
        p = 0 - p;
    }
    let g = gcd(a, b);
    return p / g;
}

/// Truncate toward zero.
fn trunc(float x) -> float {
    if x >= 0.0 {
        return floor(x);
    }
    return ceil(x);
}

/// Fractional part (`x - trunc(x)`).
fn fract(float x) -> float {
    return x - trunc(x);
}

/// True when `x` is NaN.
fn is_nan(float x) -> bool {
    return x != x;
}

/// True when `x` is infinite (not NaN).
fn is_infinite(float x) -> bool {
    if is_nan(x) {
        return false;
    }
    let big = 1.0;
    let i = 0;
    while i < 400 {
        big = big * 10.0;
        i = i + 1;
    }
    return x == big || x == 0.0 - big;
}

/// True when `x` is neither NaN nor infinite.
fn is_finite(float x) -> bool {
    return !is_nan(x) && !is_infinite(x);
}

/// Euclidean remainder (`0 <= rem < abs(b)` when `b != 0`).
fn rem_euclid(int a, int b) -> int {
    let r = a % b;
    if r < 0 {
        return r + abs(b);
    }
    return r;
}

/// Euclidean division quotient.
fn div_euclid(int a, int b) -> int {
    let r = rem_euclid(a, b);
    return (a - r) / b;
}

/// Hypotenuse `sqrt(a*a + b*b)`.
fn hypot(float a, float b) -> float {
    return sqrt(a * a + b * b);
}

test("lcm multiplies ints after float overloads") {
    assert(lcm(4, 6) == 12)?;
    assert(lcm(0 - 4, 6) == 12)?;
}
