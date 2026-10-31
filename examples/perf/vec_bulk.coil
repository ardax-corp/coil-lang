// Counted copy and fill loops over `Vec`: a byte copy and a byte fill of
// 1 MiB, an int copy of 128 Ki elements, a copy of a 1 MiB `string`'s
// bytes, and a copy of 1 MiB up to a quote. Prints ns per call; the
// compiler runs each loop as one call (`mir/bulk.rs`), the last as a
// byte scan and one call (`mir/copy_until.rs`).
use clock::mono_nanos;
use io::{stdout, write};
use string::{format, from_bytes, to_bytes};

fn say(string s) {
    let _ = write(stdout(), to_bytes(s));
}

fn copy_bytes(Vec<byte> src, int from) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = from;
    while i < src.len() {
        out.push(src[i]);
        i = i + 1;
    }
    return out;
}

fn fill_bytes(byte b, int n) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = 0;
    while i < n {
        out.push(b);
        i = i + 1;
    }
    return out;
}

fn copy_ints(Vec<int> src, int from) -> Vec<int> {
    let out: Vec<int> = Vec::new();
    let i = from;
    while i < src.len() {
        out.push(src[i]);
        i = i + 1;
    }
    return out;
}

fn copy_str(string s, int from) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = from;
    while i < s.len() {
        out.push(s[i]);
        i = i + 1;
    }
    return out;
}

fn copy_until_quote(Vec<byte> src, int from) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = from;
    while i < src.len() {
        let c = src[i];
        if c == "\"" || c == "\\" {
            break;
        }
        out.push(c);
        i = i + 1;
    }
    return out;
}

fn main() {
    let mib = 1048576;
    let bytes = fill_bytes(97 as byte, mib);
    let nums: Vec<int> = Vec::new();
    let j = 0;
    while j < 131072 {
        nums.push(j * 3);
        j = j + 1;
    }
    let reps = 20;
    let acc = 0;
    let t0 = mono_nanos();
    let k = 0;
    while k < reps {
        acc = acc + copy_bytes(bytes, k % 3).len();
        k = k + 1;
    }
    let t1 = mono_nanos();
    k = 0;
    while k < reps {
        acc = acc + fill_bytes(98 as byte, mib - k % 3).len();
        k = k + 1;
    }
    let t2 = mono_nanos();
    k = 0;
    while k < reps {
        acc = acc + copy_ints(nums, k % 3).len();
        k = k + 1;
    }
    let t3 = mono_nanos();
    let text = from_bytes(bytes).unwrap_or("");
    k = 0;
    while k < reps {
        acc = acc + copy_str(text, k % 3).len();
        k = k + 1;
    }
    let t4 = mono_nanos();
    let quoted = fill_bytes(97 as byte, mib);
    quoted.push(34 as byte);
    k = 0;
    while k < reps {
        acc = acc + copy_until_quote(quoted, k % 3).len();
        k = k + 1;
    }
    let t5 = mono_nanos();
    say(format("acc=%i\n", acc));
    say(format("copy bytes 1 MiB  %i ns\n", (t1 - t0) / reps));
    say(format("fill bytes 1 MiB  %i ns\n", (t2 - t1) / reps));
    say(format("copy ints 128 Ki  %i ns\n", (t3 - t2) / reps));
    say(format("copy str 1 MiB    %i ns\n", (t4 - t3) / reps));
    say(format("copy until 1 MiB  %i ns\n", (t5 - t4) / reps));
}
