// `byte` arithmetic wraps modulo 256 (coil-lang#882): `+ - * ** <<` and
// negation keep the low 8 bits, so a stored byte is always in 0..=255 and
// compares and divides as one. Each also warns E0130.

class Cell {
    pub v: byte,
}

fn add(byte a, byte b) -> byte {
    return a + b;
}

fn sub(byte a, byte b) -> byte {
    return a - b;
}

fn mul(byte a, byte b) -> byte {
    return a * b;
}

fn shl(byte a, int n) -> byte {
    return a << n;
}

fn pw(byte a, byte e) -> byte {
    return a ** e;
}

fn neg(byte a) -> byte {
    return -a;
}

fn b(int n) -> byte {
    return n as byte;
}

test("binary operators wrap") {
    let c = add(b(200), b(100));
    assert(c == b(44))?;
    assert(c / b(2) == b(22))?;
    assert(sub(b(100), b(200)) == b(156))?;
    assert(mul(b(200), b(100)) == b(32))?;
    assert(shl(b(200), 3) == b(64))?;
    assert(neg(b(3)) == b(253))?;
    assert(neg(b(0)) == b(0))?;
}

test("power wraps without an int overflow") {
    assert(pw(b(3), b(6)) == b(217))?;
    assert(pw(b(3), b(200)) == b(161))?;
    assert(pw(b(3), b(255)) == b(171))?;
    assert(pw(b(2), b(7)) == b(128))?;
    assert(pw(b(2), b(8)) == b(0))?;
    assert(pw(b(0), b(0)) == b(1))?;
    assert(pw(b(255), b(2)) == b(1))?;
}

test("compound assignment wraps in every place") {
    let x = b(250);
    x += b(10);
    assert(x == b(4))?;
    x -= b(5);
    assert(x == b(255))?;
    x **= b(4);
    assert(x == b(1))?;
    let cell = new Cell(b(250));
    cell.v += b(10);
    assert(cell.v == b(4))?;
    let arr: [byte; 2] = [b(200), b(1)];
    arr[0] += b(100);
    assert(arr[0] == b(44))?;
    let v: Vec<byte> = Vec::new();
    v.push(b(255));
    assert(v[0] + b(2) == b(1))?;
}

test("a wrapping loop stays in range") {
    let s = b(0);
    let i = 0;
    while i < 1000 {
        s = s + b(7);
        i += 1;
    }
    assert(s == b(88))?;
    assert((s as int) < 256)?;
}
