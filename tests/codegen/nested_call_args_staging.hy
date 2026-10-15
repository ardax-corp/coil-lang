// Nested HostInvoke call args must stage through temps — evaluating the second
// `to_bytes` inline must not clobber the first result on the operand stack.
// Output: GET /hi
use io::{stdout, write};
use string::{format, to_bytes};

fn concat_bytes(Vec<byte> a, Vec<byte> b) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = 0;
    while i < len(a) {
        out.push(a[i]);
        i = i + 1;
    }
    let j = 0;
    while j < len(b) {
        out.push(b[j]);
        j = j + 1;
    }
    return out;
}

fn main() {
    let msg = concat_bytes(to_bytes("GET "), to_bytes("/hi"));
    write(stdout(), msg);
}
