// Packed `bool` vectors and byte string literals cast to byte arrays in
// bodies lowered straight from HIR to MIR: a `Vec<bool>` push, index, store
// and loop see each element's word, and `"..." as Vec<byte>` / `as [byte; N]`
// is the literal's own array.
use tests::assert;

fn sieve(int n) -> int {
    let flags: Vec<bool> = Vec::new();
    let i = 0;
    while i < n {
        flags.push(i >= 2);
        i = i + 1;
    }
    let p = 2;
    while p * p < n {
        if flags[p] {
            let m = p * p;
            while m < n {
                flags[m] = false;
                m = m + p;
            }
        }
        p = p + 1;
    }
    let count = 0;
    for f in flags {
        if f {
            count = count + 1;
        }
    }
    return count;
}

fn bytes_sum() -> int {
    let v = "az\n" as Vec<byte>;
    let a = "ok" as [byte; 2];
    return (v[0] as int) + (v[1] as int) + (v[2] as int) + (a[1] as int) + len(v) * 1000;
}

test("bool vector sieve") {
    assert!(sieve(30) == 10);
    assert!(sieve(100) == 25);
}

test("byte literal casts") {
    assert!(bytes_sum() == 3000 + 97 + 122 + 10 + 107);
}
