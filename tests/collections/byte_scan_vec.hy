// Early-exit loops over a `Vec<byte>` skip 64 iterations at a time where
// the body does nothing (`mir/byte_scan.rs`), as string loops do. Each
// loop is checked at every mark position around the 64-byte lanes, from
// every start, and at the ends.
use tests::assert;
use string::to_bytes;
use thread::{join, spawn, ThreadError};

fn find_byte(Vec<byte> s, int b, int from) -> int {
    let i = from;
    while i < s.len() {
        if s[i] as int == b {
            return i;
        }
        i = i + 1;
    }
    return -1;
}

fn find_byte_le(Vec<byte> s, byte b, int from) -> int {
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

fn skip_ws(Vec<byte> s, int from) -> int {
    let i = from;
    while i < s.len() {
        let c = s[i];
        if c != 32 as byte && c != 9 as byte && c != 10 as byte && c != 13 as byte {
            return i;
        }
        i = i + 1;
    }
    return i;
}

fn count(Vec<byte> s, byte b) -> int {
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

fn rfind(Vec<byte> s, byte b) -> int {
    let i = s.len() - 1;
    while i >= 0 {
        if s[i] == b {
            return i;
        }
        i = i - 1;
    }
    return -1;
}

/// How many bytes at `a` and `b` agree, up to `max` (deflate's match length).
fn match_len(Vec<byte> s, int a, int b, int max) -> int {
    let k = 0;
    while k < max && s[a + k] == s[b + k] {
        k = k + 1;
    }
    return k;
}

/// `n` bytes of `x` with `mark` at `at` (none when `at` is out of range).
fn with_mark(int n, int at, byte mark) -> Vec<byte> {
    let s: Vec<byte> = Vec::new();
    let i = 0;
    while i < n {
        s.push(if i == at {
            mark
        } else {
            120 as byte
        });
        i = i + 1;
    }
    return s;
}

test("find a byte at every position and start") {
    let sizes = [0, 1, 63, 64, 65, 127, 128, 129, 200];
    for n in sizes {
        let at = 0;
        while at < n {
            let s = with_mark(n, at, 44 as byte);
            let from = 0;
            while from <= n {
                let want = if from <= at {
                    at
                } else {
                    -1
                };
                assert!(find_byte(s, 44, from) == want);
                assert!(find_byte_le(s, 44 as byte, from) == want);
                from = from + if from < 4 || from > n - 4 {
                    1
                } else {
                    7
                };
            }
            assert!(count(s, 44 as byte) == 1);
            assert!(rfind(s, 44 as byte) == at);
            at = at + 1;
        }
        assert!(find_byte(with_mark(n, -1, 44 as byte), 44, 0) == -1);
        assert!(rfind(with_mark(n, -1, 44 as byte), 44 as byte) == -1);
    }
}

test("int needles outside a byte never match") {
    let s = with_mark(150, 140, 44 as byte);
    assert!(find_byte(s, 44 + 256, 0) == -1);
    assert!(find_byte(s, -212, 0) == -1);
    assert!(find_byte(s, 44, 0) == 140);
}

test("a byte class over bytes from a string") {
    let pad = "                                                                ";
    let s = to_bytes(pad + "\t\r\n" + pad + "x" + pad);
    assert!(skip_ws(s, 0) == 131);
    assert!(skip_ws(s, 131) == 131);
    assert!(skip_ws(s, 132) == s.len());
    assert!(count(s, 32 as byte) == 192);
}

test("match length at every length around the lanes") {
    let lens = [0, 1, 63, 64, 65, 130, 258];
    for m in lens {
        // Two copies of a run, the second cut short by one changed byte.
        let s: Vec<byte> = Vec::new();
        let i = 0;
        while i < 300 {
            s.push((i % 7) as byte);
            i = i + 1;
        }
        let j = 0;
        while j < 300 {
            s.push(if j == m {
                200 as byte
            } else {
                (j % 7) as byte
            });
            j = j + 1;
        }
        assert!(match_len(s, 0, 300, 258) == if m < 258 {
            m
        } else {
            258
        });
    }
}

// The scalar iterations store ahead of the scan: each scan must read the
// array as it is then.
fn chain(Vec<byte> s, int step) -> int {
    let n = 0;
    let i = 0;
    while i < s.len() {
        if s[i] == 1 as byte {
            n = n + 1;
            if i + step < s.len() {
                s[i + step] = 1 as byte;
            }
        }
        i = i + 1;
    }
    return n;
}

test("stores made by earlier iterations are seen") {
    let s = with_mark(1000, 3, 1 as byte);
    assert!(chain(s, 70) == 15);
    assert!(count(s, 1 as byte) == 15);
}

// The bound is another array's length, which a scalar iteration grows.
fn grow_to(Vec<byte> s, Vec<byte> out) -> int {
    let i = 0;
    while i < out.len() {
        if s[i] == 1 as byte {
            out.push(0 as byte);
        }
        i = i + 1;
    }
    return i;
}

test("a bound that grows inside the loop") {
    let s = with_mark(400, 100, 1 as byte);
    let j = 0;
    while j < 400 {
        if j % 50 == 0 {
            s[j] = 1 as byte;
        }
        j = j + 1;
    }
    let out = with_mark(101, -1, 0 as byte);
    // 101 elements, then one more for each 1 `i` reaches: 0, 50 and 100.
    assert!(grow_to(s, out) == 104);
}

fn past_end() -> int {
    return match_len(with_mark(200, -1, 0 as byte), 0, 100, 150);
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

test("a read past the end still panics") {
    assert!(panics(past_end));
}
