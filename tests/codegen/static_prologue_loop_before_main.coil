// With a module static the setup prologue jumps to `main`; the loops of the
// function before `main` keep their own back edges (coil-lang#907).
static let ROWS: int = 3;

fn fill(Vec<int> px, int w, int h) {
    let y = 0;
    while y < h {
        let x = 0;
        while x < w {
            px[y * w + x] = y * 10 + x;
            x = x + 1;
        }
        y = y + 1;
    }
}

fn main() {
    let px: Vec<int> = Vec::new();
    let i = 0;
    while i < 4 * ROWS {
        px.push(0);
        i = i + 1;
    }
    fill(px, 4, ROWS);
    if px[11] != 23 {
        panic "assertion failed";
    }
}
