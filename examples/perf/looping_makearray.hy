// S2d: preheader MakeArray + computed-index store (maps + dense).
// 200000 stores; checksum 200000 (panic on mismatch).
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
    // Through a function value: a direct call would be computed while
    // compiling (-O2 folds pure calls with constant arguments).
    let run = black_box(bump);
    if run() != 200000 {
        panic "looping_makearray checksum";
    }
}
