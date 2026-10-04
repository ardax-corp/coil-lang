// Bodies the HIR lowering covers since `byte` scalars: byte locals and
// one-byte string literals, casts between scalars, and the `()` binding
// of `?` on a unit `Result`. The language harness also runs under
// `--hir`, so each case pins both codegens.
use string::to_bytes;

fn hex_val(byte c) -> int {
    if c >= "0" && c <= "9" {
        return (c as int) - (("0" as byte) as int);
    }
    if c >= "a" && c <= "f" {
        return (c as int) - (("a" as byte) as int) + 10;
    }
    return -1;
}

fn shout(Vec<byte> buf) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = 0;
    while i < len(buf) {
        let c = buf[i];
        if c >= "a" && c <= "z" {
            let upper = (c as int) - 32;
            out.push(upper as byte);
        } else {
            out.push(c);
        }
        i = i + 1;
    }
    return out;
}

fn mix(int n) -> float {
    let f = n as float;
    let back = (f * 2.5) as int;
    let flag = back as bool;
    return f + (flag as int) as float;
}

fn check(int n) -> Result<(), string> {
    if n < 0 {
        raise "negative";
    }
    return Result::Ok(());
}

fn twice(int n) -> Result<int, string> {
    check(n)?;
    check(n - 1)?;
    return Result::Ok(n * 2);
}

test("byte literals and casts") {
    assert(hex_val("7") == 7)?;
    assert(hex_val("c") == 12)?;
    assert(hex_val("z") == -1)?;
}

test("byte buffers") {
    let up = shout(to_bytes("hi there"));
    assert(len(up) == 8)?;
    assert(up[0] as int == 72)?;
    assert(up[2] as int == 32)?;
}

test("scalar casts") {
    assert(mix(2) == 3.0)?;
    assert(mix(0) == 0.0)?;
}

test("unit try") {
    assert(twice(3) == Result::Ok(6))?;
    assert(twice(1) == Result::Ok(2))?;
    match twice(0) {
        Result::Ok(_) => assert(false)?,
        Result::Err(e) => assert(e == "negative")?,
    }
}
