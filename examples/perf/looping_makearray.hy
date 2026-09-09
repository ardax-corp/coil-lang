// S2d: preheader MakeArray + computed-index store (maps + dense).
// 200000 stores; checksum 200000 (raise on mismatch).
fn bump() -> int {
    let arr = [0, 0, 0, 0];
    let i = 0;
    while i < 200000 {
        arr[i % 4] = arr[i % 4] + 1;
        i = i + 1;
    }
    return arr[0] + arr[1] + arr[2] + arr[3];
}

fn main() {
    if bump() != 200000 {
        raise "looping_makearray checksum";
    }
}
