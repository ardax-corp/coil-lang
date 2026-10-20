// Control: preheader MakeArray + computed-index store (landed S2d win).
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
        panic "s2d_preheader_bump checksum";
    }
}
