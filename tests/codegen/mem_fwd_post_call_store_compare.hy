// Nested CALL + `let x = f(); if x == k` must not hang: mem_fwd must not
// turn StorePop;Load into Dup;Store when the store extends tell past TOS
// (shared-stack CmpJmpf would eat the local — broke http `parse_url`).
// Output: 18
use io::{stdout, write};
use string::{format, to_bytes};

fn find_bytes(Vec<byte> hay, Vec<byte> needle) -> int {
    let hn = len(hay);
    let nn = len(needle);
    if nn == 0 {
        return 0;
    }
    if nn > hn {
        return 999999;
    }
    let i = 0;
    while i + nn <= hn {
        let ok = 1;
        let j = 0;
        while j < nn {
            if hay[i + j] != needle[j] {
                ok = 0;
            }
            j = j + 1;
        }
        if ok == 1 {
            return i;
        }
        i = i + 1;
    }
    return 999999;
}

fn bytes_slice(Vec<byte> src, int start, int end) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = start;
    while i < end {
        if i < len(src) {
            out.push(src[i]);
        }
        i = i + 1;
    }
    return out;
}

fn parse_url(string s) -> int {
    let b = to_bytes(s);
    let sep = Vec::from([58 as byte, 47 as byte, 47 as byte]);
    let sep_at = find_bytes(b, sep);
    if sep_at == 999999 {
        return -1;
    }
    let scheme_b = bytes_slice(b, 0, sep_at);
    let rest_start = sep_at + 3;
    let host_end = len(b);
    let host_b = bytes_slice(b, rest_start, host_end);
    return len(scheme_b) + len(host_b);
}

fn main() {
    write(stdout(), to_bytes(format("%i", parse_url("http://example.com/hi"))));
}
