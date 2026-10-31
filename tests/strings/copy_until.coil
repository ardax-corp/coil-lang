// A loop that pushes each byte until one stops it runs as a scan to the
// stop and one copy of the run (`mir/copy_until.rs`). Each is checked
// against the same loop kept scalar (`black_box` in its body).
use tests::assert;
use string::to_bytes;

fn until_quote(Vec<byte> b, Vec<byte> out, int from) -> int {
    let i = from;
    while i < b.len() {
        let c = b[i];
        if c == "\"" || c == "\\" {
            break;
        }
        out.push(c);
        i = i + 1;
    }
    return i;
}

fn until_quote_scalar(Vec<byte> b, Vec<byte> out, int from) -> int {
    let i = from;
    while i < b.len() {
        let c = black_box(b[i]);
        if c == "\"" || c == "\\" {
            break;
        }
        out.push(c);
        i = i + 1;
    }
    return i;
}

fn until_comma(string s, Vec<byte> out, int from) -> int {
    let i = from;
    while i < s.len() {
        let c = s[i];
        if c == "," || c == "\n" {
            break;
        }
        out.push(c);
        i = i + 1;
    }
    return i;
}

fn until_comma_scalar(string s, Vec<byte> out, int from) -> int {
    let i = from;
    while i < s.len() {
        let c = black_box(s[i]);
        if c == "," || c == "\n" {
            break;
        }
        out.push(c);
        i = i + 1;
    }
    return i;
}

// A range test over two blocks, a value bound, and int elements.
fn digits(string s, Vec<int> out, int from, int to) -> int {
    let i = from;
    while i < to {
        let c = s[i];
        if c < "0" || c > "9" {
            break;
        }
        out.push(c as int);
        i = i + 1;
    }
    return i;
}

fn digits_scalar(string s, Vec<int> out, int from, int to) -> int {
    let i = from;
    while i < to {
        let c = black_box(s[i]);
        if c < "0" || c > "9" {
            break;
        }
        out.push(c as int);
        i = i + 1;
    }
    return i;
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

fn text(int n, int stop) -> Vec<byte> {
    let b: Vec<byte> = Vec::new();
    let k = 0;
    while k < n {
        b.push(
            if k == stop {
                34 as byte
            } else {
                (97 + k % 26) as byte
            },
        );
        k = k + 1;
    }
    return b;
}

test("copy until a quote on a byte array") {
    for n in [0, 1, 7, 63, 64, 65, 200] {
        for stop in [-1, 0, 1, 40, n - 1] {
            let b = text(n, stop);
            for from in [0, 1, 30] {
                let x = to_bytes("<");
                let y = to_bytes("<");
                assert!(until_quote(b, x, from) == until_quote_scalar(b, y, from));
                assert!(same_bytes(x, y));
            }
        }
    }
    let b = to_bytes("ab\\cd\"e");
    let out = to_bytes("");
    assert!(until_quote(b, out, 0) == 2);
    assert!(until_quote(b, out, 3) == 5);
    assert!(same_bytes(out, to_bytes("abcd")));
}

test("copy until a comma on a string") {
    let s = "field one,two\nthree four five six seven eight nine ten eleven twelve,x";
    let from = 0;
    while from <= s.len() {
        let x = to_bytes("");
        let y = to_bytes("");
        assert!(until_comma(s, x, from) == until_comma_scalar(s, y, from));
        assert!(same_bytes(x, y));
        from = from + 1;
    }
}

test("copy digits with a value bound onto ints") {
    let s = "12345678901234567890123x45";
    for to in [0, 3, 20, 23, 26] {
        for from in [0, 2, 22, 24] {
            let x: Vec<int> = [-1];
            let y: Vec<int> = [-1];
            assert!(digits(s, x, from, to) == digits_scalar(s, y, from, to));
            assert!(x == y);
        }
    }
}

fn past_end() -> int {
    let out: Vec<int> = Vec::new();
    return digits("123", out, 1, 9);
}

test("a read past the end still panics") {
    assert!(panics(past_end));
}

use thread::{join, spawn, ThreadError};

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
