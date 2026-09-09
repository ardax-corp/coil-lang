// In-loop MakeArray + Index + StoreIndex (computed index).
// N=2000000; checksum 1999999000000 (sum 0..n-1).
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
    if pack(2000000) != 1999999000000 {
        raise "s2d_inloop_pack_store checksum";
    }
}
