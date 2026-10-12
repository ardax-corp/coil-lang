// `Vec<byte>` / `Vec<bool>` are stored one byte per element (#927). Reads,
// writes, growth, generic bodies and collections must see the same values
// as word storage, and a byte vector must cost about a byte per element.
use gc::{collect, heap_bytes};
use string::to_bytes;

fn push_all<T>(Vec<T> v, T x, int n) {
    let i = 0;
    while i < n {
        v.push(x);
        i = i + 1;
    }
}

fn first<T>(Vec<T> v) -> T {
    return v[0];
}

test("byte vectors round-trip every value") {
    let v: Vec<byte> = Vec::new();
    let i = 0;
    while i < 256 {
        v.push(i as byte);
        i = i + 1;
    }
    collect();
    let sum = 0;
    i = 0;
    while i < len(v) {
        sum = sum + v[i] as int;
        i = i + 1;
    }
    assert(sum == 32640)?;
    v[3] = 250 as byte;
    assert(v[3] as int == 250)?;
    v.insert(0, 7 as byte);
    assert(v[0] as int == 7 && v[4] as int == 250)?;
    assert(len(v) == 257)?;
}

test("literal, with_capacity and from") {
    let lit: Vec<byte> = [1 as byte, 2 as byte, 255 as byte];
    assert(lit[2] as int == 255)?;
    let cap: Vec<byte> = Vec::with_capacity(4);
    cap.push(9 as byte);
    assert(len(cap) == 1 && cap[0] as int == 9)?;
    let fixed = [1 as byte, 2 as byte, 255 as byte];
    let copy: Vec<byte> = Vec::from(fixed);
    copy[0] = 100 as byte;
    assert(copy[0] as int == 100 && fixed[0] as int == 1)?;
    let expect: Vec<byte> = [100 as byte, 2 as byte, 255 as byte];
    assert(copy == expect)?;
    assert(copy != lit)?;
    let words = to_bytes("hi");
    let packed: Vec<byte> = Vec::new();
    packed.push(104 as byte);
    packed.push(105 as byte);
    assert(words == packed && packed == words)?;
}

test("bool vectors") {
    let flags: Vec<bool> = Vec::new();
    let i = 0;
    while i < 10 {
        flags.push(i % 3 == 0);
        i = i + 1;
    }
    let count = 0;
    i = 0;
    while i < len(flags) {
        if flags[i] {
            count = count + 1;
        }
        i = i + 1;
    }
    assert(count == 4)?;
    flags[1] = true;
    assert(flags[1] && !flags[2])?;
}

test("generic bodies read and write packed vectors") {
    let v: Vec<byte> = Vec::new();
    push_all(v, 42 as byte, 5);
    assert(len(v) == 5 && v[4] as int == 42)?;
    assert(first(v) as int == 42)?;
    let b: Vec<bool> = [false, true];
    push_all(b, true, 2);
    assert(len(b) == 4 && b[3] && !first(b))?;
}

test("a byte vector costs about a byte per element") {
    collect();
    let before = heap_bytes();
    let v: Vec<byte> = Vec::with_capacity(100000);
    let i = 0;
    while i < 100000 {
        v.push((i % 256) as byte);
        i = i + 1;
    }
    let grown = heap_bytes() - before;
    assert(grown < 200000)?;
    assert(v[99999] as int == 99999 % 256)?;
}

test("element-wise matrix ops read packed rows") {
    let q = matrix([[34 as byte, 0 as byte]]);
    let flipped = ~q;
    assert(flipped[0][0] as int == 221 && flipped[0][1] as int == 255)?;
}
