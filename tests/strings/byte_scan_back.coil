// Count-down byte loops (`iv = iv - 1` against a lower bound) skip 64
// iterations at a time too (`mir/byte_scan.rs`, #1968). Each is checked
// against a plain backward search at every position around the lanes.
use tests::assert;
use thread::{join, spawn, ThreadError};

fn rfind_byte(string s, byte b) -> int {
    let i = s.len() - 1;
    while i >= 0 {
        if s[i] == b {
            return i;
        }
        i = i - 1;
    }
    return -1;
}

// `i > lo` with the bound on the left, stepping by `+ -1`.
fn rfind_above(string s, byte b, int from, int lo) -> int {
    let i = from;
    while lo < i {
        if s[i] == b {
            return i;
        }
        i = i + -1;
    }
    return -1;
}

fn last_not(string s, byte b) -> int {
    let i = s.len() - 1;
    while i >= 0 {
        if s[i] != b {
            return i;
        }
        i = i - 1;
    }
    return -1;
}

// The loop exits on `i < 0` rather than running while `i >= 0`.
fn rfind_exit(string s, byte b) -> int {
    let i = s.len() - 1;
    while !(i < 0) {
        if s[i] == b {
            return i;
        }
        i = i - 1;
    }
    return -1;
}

// Two streams: a needle's first and last bytes.
fn rfind_pair(string s, byte a, byte b) -> int {
    let i = s.len() - 2;
    while i >= 0 {
        if s[i] == a && s[i + 1] == b {
            return i;
        }
        i = i - 1;
    }
    return -1;
}

fn reference(string s, string needle, int from, int lo) -> int {
    let m = needle.len();
    let i = from;
    while i > lo {
        let j = 0;
        while j < m && i + j < s.len() && s[i + j] == needle[j] {
            j = j + 1;
        }
        if j == m {
            return i;
        }
        i = i - 1;
    }
    return -1;
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

test("find the last byte at every position") {
    let sizes = [0, 1, 63, 64, 65, 127, 128, 129, 200];
    for n in sizes {
        let at = 0;
        while at < n {
            let s = with_mark(n, at, ",");
            assert!(rfind_byte(s, 44 as byte) == at);
            assert!(rfind_exit(s, 44 as byte) == at);
            assert!(last_not(s, 120 as byte) == at);
            at = at + 1;
        }
        assert!(rfind_byte(with_mark(n, -1, ","), 44 as byte) == -1);
        assert!(last_not(with_mark(n, -1, ","), 120 as byte) == -1);
    }
}

test("the last of several matches wins") {
    let s = with_mark(150, 10, ",") + with_mark(150, 70, ",") + with_mark(150, 140, ",");
    assert!(rfind_byte(s, 44 as byte) == 440);
    assert!(rfind_above(s, 44 as byte, 439, -1) == 220);
    assert!(rfind_above(s, 44 as byte, 219, -1) == 10);
}

test("every start and lower bound") {
    let n = 200;
    let at = 0;
    while at < n {
        let s = with_mark(n, at, ";");
        let from = n - 1;
        while from >= 0 {
            let lo = -1;
            while lo < from {
                assert!(rfind_above(s, 59 as byte, from, lo) == reference(s, ";", from, lo));
                lo = lo + if lo < 4 || lo > from - 4 {
                    1
                } else {
                    23
                };
            }
            from = from - if from < 4 || from > n - 4 {
                1
            } else {
                11
            };
        }
        at = at + 7;
    }
}

test("a two-byte filter at every position") {
    let sizes = [2, 63, 64, 65, 66, 129, 200];
    for n in sizes {
        let at = 0;
        while at + 2 <= n {
            let s = with_mark(n - 1, at, ";,");
            assert!(rfind_pair(s, 59 as byte, 44 as byte) == reference(s, ";,", s.len() - 1, -1));
            assert!(rfind_pair(s, 44 as byte, 59 as byte) == reference(s, ",;", s.len() - 1, -1));
            at = at + 1;
        }
    }
}

test("bounds near the int limits do not overflow") {
    let s = with_mark(300, 50, ";");
    let lo = -9223372036854775807 - 1;
    assert!(rfind_above(s, 59 as byte, 299, lo) == 50);
    assert!(rfind_above(s, 59 as byte, 299, 50) == -1);
    assert!(rfind_above(s, 59 as byte, 299, 49) == 50);
    assert!(rfind_above(s, 59 as byte, 299, 9223372036854775807) == -1);
}

fn from_past_end() -> int {
    return rfind_above(with_mark(200, 50, ","), 44 as byte, 203, -1);
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

test("a start past the string still panics") {
    assert!(panics(from_past_end));
}
