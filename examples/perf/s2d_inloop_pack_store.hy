// In-loop MakeArray + Index + StoreIndex (computed index) — S2f SROA hit.
// N=2000000; checksum n*(n-1)/2 = 1999999000000.
fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        let xs = [0, 0, 0];
        xs[i % 3] = i;
        s = s + xs[i % 3];
        i = i + 1;
    }
    return s;
}

fn main() {
    let n = 2000000;
    if pack(6) != 15 {
        raise "s2d_inloop_pack_store pack(6)";
    }
    if pack(n) != n * (n - 1) / 2 {
        raise "s2d_inloop_pack_store checksum";
    }
}
