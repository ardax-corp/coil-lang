// `s[i]` in a dense loop reads the byte in place (`DenseIndex` with
// `HEAP_STR_BYTE`): the same bytes as `to_bytes`, the out-of-range panic,
// and a string a closure captures.
use tests::assert;
use string::to_bytes;
use thread::{join, spawn, ThreadError};

fn count(string s, byte b) -> int {
    let n = 0;
    let i = 0;
    while i < s.len() {
        if s[i] == b {
            n = n + 1;
        }
        i = i + 1;
    }
    return n;
}

fn sum_to(string s, int end) -> int {
    let t = 0;
    let i = 0;
    while i < end {
        t = t + s[i] as int;
        i = i + 1;
    }
    return t;
}

fn past_end() -> int {
    return sum_to("abc", 4);
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

test("dense string bytes") {
    let s = "a,b,,c" + ",é";
    assert!(count(s, 44 as byte) == 4);
    assert!(sum_to(s, s.len()) == 97 + 98 + 99 + 44 * 4 + 195 + 169);
    let b = to_bytes(s);
    let i = 0;
    while i < s.len() {
        assert!(s[i] == b[i]);
        i = i + 1;
    }
}

test("captured string") {
    let s = "xyz";
    s = s + "!";
    let f = fn () use (s) => sum_to(s, 4);
    assert!(f() == 120 + 121 + 122 + 33);
}

test("past the end panics") {
    assert!(panics(past_end));
}
