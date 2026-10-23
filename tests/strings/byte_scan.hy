// Early-exit byte loops skip 64 iterations at a time where the body does
// nothing (`mir/byte_scan.rs`). Each loop here is checked against the
// `find_from` native at every needle position around the 64-byte lanes,
// from every start, and at the ends.
use tests::assert;
use string::{find_from, slice_bytes};
use thread::{join, spawn, ThreadError};

fn find_byte(string s, int b, int from) -> int {
    let i = from;
    while i < s.len() {
        if s[i] as int == b {
            return i;
        }
        i = i + 1;
    }
    return -1;
}

fn find_byte_le(string s, byte b, int from) -> int {
    let n = s.len() - 1;
    let i = from;
    while i <= n {
        if s[i] == b {
            return i;
        }
        i = i + 1;
    }
    return -1;
}

fn find_either(string s, byte a, byte b) -> int {
    let i = 0;
    while i < s.len() {
        if s[i] == a || s[i] == b {
            return i;
        }
        i = i + 1;
    }
    return -1;
}

fn first_digit(string s) -> int {
    let i = 0;
    while i < s.len() {
        let c = s[i];
        if c >= 48 as byte && c <= 57 as byte {
            return i;
        }
        i = i + 1;
    }
    return -1;
}

fn first_not(string s, byte b) -> int {
    let i = 0;
    while i < s.len() {
        if s[i] != b {
            return i;
        }
        i = i + 1;
    }
    return -1;
}

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

fn find_str(string s, string pat, int from) -> int {
    let m = pat.len();
    let n = s.len() - m;
    let first = pat[0];
    let i = from;
    while i <= n {
        if s[i] == first {
            let j = 1;
            while j < m && s[i + j] == pat[j] {
                j = j + 1;
            }
            if j == m {
                return i;
            }
        }
        i = i + 1;
    }
    return -1;
}

fn reference_count(string s, string b) -> int {
    let n = 0;
    let at = find_from(s, b, 0);
    while at >= 0 {
        n = n + 1;
        at = find_from(s, b, at + 1);
    }
    return n;
}

fn filler(int n) -> string {
    let unit = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZé";
    let s = "";
    while s.len() < n {
        s = s + unit;
    }
    return match slice_bytes(s, 0, n) {
        Result::Ok(t) => t,
        Result::Err(_) => s,
    };
}

/// `n` filler bytes with `mark` at `at`.
fn with_mark(int n, int at, string mark) -> string {
    let s = "";
    let i = 0;
    while i < n {
        s = s + if i == at {
            mark
        } else {
            "x"
        };
        i = i + 1;
    }
    return s;
}

test("find a byte at every position and start") {
    let sizes = [0, 1, 63, 64, 65, 127, 128, 129, 200];
    for n in sizes {
        let at = 0;
        while at < n {
            let s = with_mark(n, at, ",");
            let from = 0;
            while from <= n {
                let want = find_from(s, ",", from);
                assert!(find_byte(s, 44, from) == want);
                assert!(find_byte_le(s, 44 as byte, from) == want);
                from = from + if from < 4 || from > n - 4 {
                    1
                } else {
                    7
                };
            }
            assert!(find_either(s, 59 as byte, 44 as byte) == at);
            assert!(first_not(s, 120 as byte) == at);
            assert!(count(s, 44 as byte) == 1);
            at = at + 1;
        }
        assert!(find_byte(with_mark(n, -1, ","), 44, 0) == -1);
    }
}

test("int needles outside a byte never match") {
    let s = with_mark(150, 140, ",");
    assert!(find_byte(s, 44 + 256, 0) == -1);
    assert!(find_byte(s, -212, 0) == -1);
    assert!(find_byte(s, 44, 0) == 140);
}

test("bytes past 127 and range tests") {
    let s = filler(300) + "7";
    assert!(first_digit(s) == find_from(s, "0", 0));
    assert!(find_byte(s, 195, 0) == find_from(s, "é", 0));
    let tail = with_mark(130, 129, "5");
    assert!(first_digit(tail) == 129);
    assert!(first_digit(with_mark(130, -1, "5")) == -1);
}

test("count matches the native") {
    let s = filler(1000);
    assert!(count(s, 97 as byte) == reference_count(s, "a"));
    assert!(count(s, 195 as byte) == reference_count(s, "é"));
    assert!(count("", 97 as byte) == 0);
}

test("substring search, short and long needles") {
    let hay = filler(700) + "needle" + filler(90);
    assert!(find_str(hay, "needle", 0) == find_from(hay, "needle", 0));
    assert!(find_str(hay, "needle", 701) == -1);
    let long = filler(130);
    let big = filler(2000) + "!" + long;
    assert!(find_str(big, "!" + long, 0) == find_from(big, "!" + long, 0));
    assert!(find_str(big, long, 5) == find_from(big, long, 5));
    let near = long + "?";
    assert!(find_str(filler(2000) + long + "!", near, 0) == -1);
}

test("a slice view searches its own bytes") {
    let base = filler(500) + "," + filler(100);
    let view = match slice_bytes(base, 100, base.len()) {
        Result::Ok(t) => t,
        Result::Err(_) => panic "slice",
    };
    assert!(find_byte(view, 44, 0) == 400);
}

test("a captured string") {
    let s = with_mark(100, 90, ";");
    s = s + "x";
    let f = fn () use (s) => find_byte(s, 59, 0);
    assert!(f() == 90);
}

fn from_before_start() -> int {
    return find_byte(with_mark(200, 150, ","), 44, -3);
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

test("a start before the string still panics") {
    assert!(panics(from_before_start));
}
