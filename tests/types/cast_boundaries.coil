// Primitive casts follow Rust: `float as int` truncates toward zero and
// saturates at the `int` limits (NaN gives 0); a non-literal `int as byte`
// keeps the low 8 bits; `byte` widens to `int` exactly.
use tests::assert;

fn to_int(float f) -> int {
    return f as int;
}

fn to_byte(int n) -> byte {
    return n as byte;
}

fn fdiv(float a, float b) -> float {
    return a / b;
}

test("float as int truncates toward zero") {
    assert!(to_int(2.9) == 2);
    assert!(to_int(-2.9) == -2);
    assert!(to_int(0.999) == 0);
    assert!(to_int(-0.999) == 0);
    assert!(to_int(-0.0) == 0);
    assert!(to_int(1000.5) == 1000);
}

test("float as int saturates and maps NaN to 0") {
    let inf = fdiv(1.0, 0.0);
    assert!(to_int(inf) == 9223372036854775807);
    assert!(to_int(-inf) == -9223372036854775807 - 1);
    assert!(to_int(fdiv(0.0, 0.0)) == 0);
    let huge = 1000000000000.0 * 1000000000000.0 * 1000000.0;
    assert!(to_int(huge) == 9223372036854775807);
    assert!(to_int(-huge) == -9223372036854775807 - 1);
}

test("int as float") {
    assert!((7 as float) == 7.0);
    let n = -3;
    assert!((n as float) == -3.0);
    let big = 9007199254740993;
    assert!((big as float) == 9007199254740992.0);
}

test("int as byte keeps the low 8 bits") {
    assert!(to_byte(0) as int == 0);
    assert!(to_byte(255) as int == 255);
    assert!(to_byte(256) as int == 0);
    assert!(to_byte(257) as int == 1);
    assert!(to_byte(-1) as int == 255);
    assert!(to_byte(-256) as int == 0);
    assert!(to_byte(-257) as int == 255);
    assert!(to_byte(4660) as int == 52);
}

test("byte widens exactly") {
    let b: byte = 200;
    assert!(b as int == 200);
    assert!((b as int) as float == 200.0);
    assert!((b as int) + 100 == 300);
}

test("literal byte casts in range") {
    assert!((0 as byte) as int == 0);
    assert!((255 as byte) as int == 255);
    assert!(("A" as byte) as int == 65);
}

test("cast binds tighter than arithmetic") {
    let n = 3;
    assert!(n as float * 1.5 == 4.5);
    assert!(1.0 + n as float == 4.0);
    let m = 300;
    let c = m as byte;
    assert!(c as int == 44);
}

test("round trips") {
    let i = 0;
    while i < 256 {
        assert!(to_byte(i) as int == i);
        assert!(to_int(i as float) == i);
        i = i + 1;
    }
}
