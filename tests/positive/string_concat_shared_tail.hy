// `a + b` may append in place when `a` ends at its buffer's tail. Appending
// to an older prefix must copy, never clobber a longer string that already
// shares the buffer.
use text::{join};

fn grow(string s, int n) -> string {
    let i = 0;
    while i < n {
        s = s + "x";
        i = i + 1;
    }
    return s;
}

test("branching from a shared prefix keeps both strings") {
    let base = "ab" + "c";
    let left = base + "L";
    let right = base + "R";
    assert(base == "abc")?;
    assert(left == "abcL")?;
    assert(right == "abcR")?;
    let again = left + "!";
    assert(again == "abcL!")?;
    assert(right + "?" == "abcR?")?;
}

test("loop growth past several buffers") {
    let s = grow("", 1000);
    assert(len(s) == 1000)?;
    let t = grow(s, 5);
    assert(len(t) == 1005)?;
    assert(len(s) == 1000)?;
    let u = s + "y";
    assert(len(u) == 1001)?;
    assert(t != u)?;
}

test("format and join build on shared strings") {
    let parts: Vec<string> = Vec::new();
    parts.push("a");
    parts.push("b");
    parts.push("c");
    let j = join(parts, ",");
    assert(j == "a,b,c")?;
    let k = j + "," + "d";
    assert(k == "a,b,c,d")?;
    assert(j == "a,b,c")?;
}
