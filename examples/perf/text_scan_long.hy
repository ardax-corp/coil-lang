// Long-line search: a rare needle at the end of a 64 KiB line, plus a
// split of a long line with sparse separators. Prints ns per call for a
// plain Coil byte loop, a plain Coil substring loop, and the stdlib
// `find` / `split`. Byte-lane SIMD (docs/internals/byte-simd.md) targets
// the plain loops.
use clock::mono_nanos;
use io::{stdout, write};
use string::{format, to_bytes};
use text::{Pattern, Searchable};

fn say(string s) {
    write(stdout(), to_bytes(s));
}

/// First `b` at or after `from`, or -1.
fn find_byte(string s, int b, int from) -> int {
    let n = s.len();
    let i = from;
    while i < n {
        if s[i] as int == b {
            return i;
        }
        i = i + 1;
    }
    return -1;
}

/// First `pat` at or after `from`, or -1 (first-byte filter, then compare).
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

fn line(int n, int every) -> string {
    let unit = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-_";
    let s = "";
    let i = 0;
    while s.len() < n {
        s = s + unit;
        i = i + 1;
        if every > 0 && i % every == 0 {
            s = s + ",";
        }
    }
    return s;
}

fn main() {
    let hay = line(65536, 0) + "needle;";
    let sparse = line(65536, 16);
    // `from` varies with `k`, so no call is loop-invariant.
    let reps = 200;

    let t0 = mono_nanos();
    let k = 0;
    let acc = 0;
    while k < reps {
        acc = acc + find_byte(hay, 59, k % 3);
        k = k + 1;
    }
    let t1 = mono_nanos();
    k = 0;
    while k < reps {
        acc = acc + find_str(hay, "needle", k % 3);
        k = k + 1;
    }
    let t2 = mono_nanos();
    k = 0;
    while k < reps {
        acc = acc + (";".find_in(hay, k % 3) ?? new text::Span(0, 0)).start;
        k = k + 1;
    }
    let t3 = mono_nanos();
    k = 0;
    while k < reps {
        acc = acc + ("needle".find_in(hay, k % 3) ?? new text::Span(0, 0)).start;
        k = k + 1;
    }
    let t4 = mono_nanos();
    k = 0;
    while k < reps {
        acc = acc + sparse.split(",").len();
        k = k + 1;
    }
    let t5 = mono_nanos();
    say(format("len=%i acc=%i\n", hay.len(), acc));
    say(format("loop find_byte   %i ns\n", (t1 - t0) / reps));
    say(format("loop find_str    %i ns\n", (t2 - t1) / reps));
    say(format("stdlib find byte %i ns\n", (t3 - t2) / reps));
    say(format("stdlib find str  %i ns\n", (t4 - t3) / reps));
    say(format("stdlib split     %i ns\n", (t5 - t4) / reps));
}
