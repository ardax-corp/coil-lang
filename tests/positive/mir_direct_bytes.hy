// Bytes in bodies lowered straight from HIR to MIR: a byte is its value as
// an int word, one-byte literals are constants, and `as byte` / `as int`
// keep the low eight bits as the VM's casts do.

fn upper(Vec<byte> buf) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = 0;
    while i < len(buf) {
        let c = buf[i];
        if c >= "a" && c <= "z" {
            let up = (c as int) - 32;
            out.push(up as byte);
        } else {
            out.push(c);
        }
        i = i + 1;
    }
    return out;
}

fn wrap(int n) -> int {
    let b = n as byte;
    let m = n + 1;
    let c = m as byte;
    let hi = b as int;
    let lo = c as int;
    return hi * 1000 + lo;
}

fn digits(Vec<byte> buf) -> int {
    let v = 0;
    for c in buf {
        if c >= "0" && c <= "9" {
            let d = c as int;
            let z = "0" as byte;
            v = v * 10 + d - (z as int);
        }
    }
    return v;
}

test("bytes in direct MIR") {
    let u = upper("hello, World" as Vec<byte>);
    assert(u[0] == "H")?;
    assert(u[5] == ",")?;
    assert(u[7] == "W")?;
    assert(len(u) == 12)?;
    assert(wrap(300) == 44045)?;
    assert(wrap(-1) == 255000)?;
    assert(digits("a1b2c3" as Vec<byte>) == 123)?;
}
