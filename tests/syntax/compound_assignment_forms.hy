// Every compound assignment `op=` updates its target like `x = x op y`,
// on locals, record fields and Vec elements; the right side is the whole
// expression to its right. (Assignment used as a value is a known bug,
// see findings.)
use tests::assert;

test("each compound operator") {
    let x = 10;
    x += 5;
    assert!(x == 15);
    x -= 3;
    assert!(x == 12);
    x *= 2;
    assert!(x == 24);
    x /= 5;
    assert!(x == 4);
    x %= 3;
    assert!(x == 1);
    x <<= 4;
    assert!(x == 16);
    x >>= 2;
    assert!(x == 4);
    x |= 3;
    assert!(x == 7);
    x &= 5;
    assert!(x == 5);
    x ^= 1;
    assert!(x == 4);
    x **= 3;
    assert!(x == 64);
}

test("right side is the whole expression") {
    let x = 2;
    x *= 1 + 2;
    assert!(x == 6);
    x -= 2 * 2;
    assert!(x == 2);
    let ok = false;
    ok = 1 < 2 && 3 < 4;
    assert!(ok);
}

test("fields and elements") {
    let d = { a: 1, b: 2 };
    d.a += 9;
    d.b *= 3;
    assert!(d.a == 10 && d.b == 6);
    let xs: Vec<int> = [1, 2, 3];
    xs[1] += 10;
    xs[2] <<= 2;
    assert!(xs[1] == 12 && xs[2] == 12);
    let i = 0;
    xs[i] -= 1;
    assert!(xs[0] == 0);
}

test("float and string compound forms") {
    let f = 1.5;
    f += 1.0;
    f *= 2.0;
    assert!(f == 5.0);
    f /= 4.0;
    assert!(f == 1.25);
    let s = "a";
    s += "b";
    s += "c" + "d";
    assert!(s == "abcd");
}
