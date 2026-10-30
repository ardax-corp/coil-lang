// Counted copy loops (`out.push(src[i])`) and fill loops (`out.push(k)`)
// run as one call (`mir/bulk.rs`). Each keeps the loop's results: the
// elements, the index after the loop, and the panic of a read out of
// range (that loop runs as written).
use tests::assert;
use string::{format, to_bytes};
use thread::{join, spawn, ThreadError};

fn copy_bytes(Vec<byte> src, Vec<byte> out, int from, int to) -> int {
    let i = from;
    while i < to {
        out.push(src[i]);
        i = i + 1;
    }
    return i;
}

fn copy_ints_le(Vec<int> src, Vec<int> out, int from, int last) -> int {
    let i = from;
    while i <= last {
        out.push(src[i]);
        i = i + 1;
    }
    return i;
}

fn copy_all(Vec<string> src) -> Vec<string> {
    let out: Vec<string> = Vec::new();
    let i = 0;
    while i < src.len() {
        out.push(src[i]);
        i = i + 1;
    }
    return out;
}

fn fill(Vec<int> out, int k, int from, int to) -> int {
    let i = from;
    while i < to {
        out.push(k);
        i = i + 1;
    }
    return i;
}

fn fill_strings(string s, int n) -> Vec<string> {
    let out: Vec<string> = Vec::new();
    let i = 0;
    while i < n {
        out.push(s);
        i = i + 1;
    }
    return out;
}

fn ints(int n) -> Vec<int> {
    let v: Vec<int> = Vec::new();
    let i = 0;
    while i < n {
        v.push(i * 1000);
        i = i + 1;
    }
    return v;
}

fn same_bytes(Vec<byte> a, Vec<byte> b) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i = i + 1;
    }
    return true;
}

test("copy a byte range onto what is there") {
    let src = to_bytes("hello, world");
    let out = to_bytes(">");
    assert!(copy_bytes(src, out, 7, 12) == 12);
    assert!(same_bytes(out, to_bytes(">world")));
    assert!(copy_bytes(src, out, 0, 5) == 5);
    assert!(same_bytes(out, to_bytes(">worldhello")));
}

test("an empty or backward range copies nothing") {
    let src = to_bytes("abc");
    let out = to_bytes("");
    assert!(copy_bytes(src, out, 2, 2) == 2);
    assert!(copy_bytes(src, out, 3, 1) == 3);
    assert!(out.len() == 0);
}

test("ints with an inclusive bound") {
    let src = ints(300);
    let out: Vec<int> = Vec::new();
    assert!(copy_ints_le(src, out, 10, 299) == 300);
    assert!(out.len() == 290);
    assert!(out[0] == 10000 && out[289] == 299000);
}

test("heap elements up to src.len()") {
    let src: Vec<string> = Vec::new();
    let i = 0;
    while i < 100 {
        src.push(format("s%i", i));
        i = i + 1;
    }
    let out = copy_all(src);
    assert!(out.len() == 100);
    assert!(out[0] == "s0" && out[57] == "s57" && out[99] == "s99");
}

test("copying an array onto itself") {
    let v = to_bytes("abc");
    assert!(copy_bytes(v, v, 0, 3) == 3);
    assert!(same_bytes(v, to_bytes("abcabc")));
}

test("fill ints and strings") {
    let out: Vec<int> = Vec::new();
    assert!(fill(out, 7, 0, 100) == 100);
    assert!(fill(out, -1, 5, 8) == 8);
    assert!(fill(out, 9, 4, 2) == 4);
    assert!(out.len() == 103);
    assert!(out[0] == 7 && out[99] == 7 && out[100] == -1 && out[102] == -1);
    let ss = fill_strings("x", 70);
    assert!(ss.len() == 70 && ss[69] == "x");
    assert!(fill_strings("x", 0).len() == 0);
}

// `out.push(s[i])` from a `string`: its bytes in one copy.
fn copy_str(string s, Vec<byte> out, int from, int to) -> int {
    let i = from;
    while i < to {
        out.push(s[i]);
        i = i + 1;
    }
    return i;
}

fn copy_str_all(string s, Vec<int> out) {
    let i = 0;
    while i < s.len() {
        out.push(s[i] as int);
        i = i + 1;
    }
}

fn copy_str_scalar(string s, Vec<byte> out, int from, int to) {
    let i = from;
    while i < to {
        out.push(black_box(s[i]));
        i = i + 1;
    }
}

test("copy a string range") {
    let s = "héllo, wörld; the quick brown fox";
    for from in [0, 1, 5, 20] {
        for to in [from, from + 1, from + 9, s.len()] {
            let x = to_bytes("ab");
            let y = to_bytes("ab");
            assert!(copy_str(s, x, from, to) == to);
            copy_str_scalar(s, y, from, to);
            assert!(same_bytes(x, y));
        }
    }
    // Onto an int array, every byte as its value.
    let w: Vec<int> = [-1];
    copy_str_all("aé", w);
    assert!(w.len() == 4 && w[0] == -1 && w[1] == 97 && w[2] == 195 && w[3] == 169);
}

fn copy_str_past_end() -> int {
    return copy_str("abcdef", to_bytes(""), 2, 9);
}

fn copy_str_before_start() -> int {
    return copy_str("abcdef", to_bytes(""), -1, 3);
}

fn copy_past_end() -> int {
    let out = to_bytes("");
    let n = copy_bytes(to_bytes("abcdef"), out, 2, 9);
    return n;
}

fn copy_before_start() -> int {
    let out = to_bytes("");
    return copy_bytes(to_bytes("abcdef"), out, -1, 3);
}

fn panics(() -> int f) -> bool {
    let t = match spawn(f) {
        Result::Ok(t) => t,
        Result::Err(_) => panic "spawn failed",
    };
    return match join(t) {
        Result::Err(ThreadError::JoinFailed) => true,
        default => false,
    };
}

test("a read out of range still panics") {
    assert!(panics(copy_past_end));
    assert!(panics(copy_before_start));
    assert!(panics(copy_str_past_end));
    assert!(panics(copy_str_before_start));
}
