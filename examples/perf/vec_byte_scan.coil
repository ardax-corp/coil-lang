// Byte loops over a `Vec<byte>`: the shapes the libraries run (find a
// byte, deflate's match length, JSON whitespace skip) on 1 MiB. Prints ns
// per call; byte-lane SIMD (docs/internals/byte-simd.md) targets them all.
use clock::mono_nanos;
use io::{stdout, write};
use string::{format, to_bytes};

fn say(string s) {
    let _ = write(stdout(), to_bytes(s));
}

/// First `b` at or after `from`, or -1.
fn find_byte(Vec<byte> s, int b, int from) -> int {
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

/// How many bytes at `a` and `b` agree, up to `max`.
fn match_len(Vec<byte> s, int a, int b, int max) -> int {
    let k = 0;
    while k < max && s[a + k] == s[b + k] {
        k = k + 1;
    }
    return k;
}

/// The first byte at or after `from` that is not JSON whitespace.
fn skip_ws(Vec<byte> s, int from) -> int {
    let n = s.len();
    let i = from;
    while i < n {
        let c = s[i];
        if c != 32 as byte && c != 9 as byte && c != 10 as byte && c != 13 as byte {
            return i;
        }
        i = i + 1;
    }
    return i;
}

fn fill(int n, byte b) -> Vec<byte> {
    let v: Vec<byte> = Vec::new();
    let i = 0;
    while i < n {
        v.push(b);
        i = i + 1;
    }
    return v;
}

fn main() {
    let mib = 1048576;
    let hay = fill(mib, 97 as byte);
    hay.push(59 as byte);
    let ws = fill(mib, 32 as byte);
    ws.push(123 as byte);
    // Two equal 32 KiB runs side by side, then one different byte.
    let lz = fill(65536, 7 as byte);
    lz.push(1 as byte);
    // `from` varies with `k`, so no call is loop-invariant.
    let reps = 50;

    let t0 = mono_nanos();
    let k = 0;
    let acc = 0;
    while k < reps {
        acc = acc + find_byte(hay, 59, k % 3);
        k = k + 1;
    }
    let t1 = mono_nanos();
    k = 0;
    while k < reps * 20 {
        acc = acc + match_len(lz, k % 3, 32768 + k % 3, 32768);
        k = k + 1;
    }
    let t2 = mono_nanos();
    k = 0;
    while k < reps {
        acc = acc + skip_ws(ws, k % 3);
        k = k + 1;
    }
    let t3 = mono_nanos();
    say(format("acc=%i\n", acc));
    say(format("find_byte 1 MiB  %i ns\n", (t1 - t0) / reps));
    say(format("match_len 32 KiB %i ns\n", (t2 - t1) / (reps * 20)));
    say(format("skip_ws 1 MiB    %i ns\n", (t3 - t2) / reps));
}
