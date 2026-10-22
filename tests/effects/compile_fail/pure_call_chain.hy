// @expected E0413 — an effect two calls down still breaks `pure` (top → middle → leaf).
static let N: int = 0;

fn leaf() -> int {
    N = N + 1;
    return N;
}

fn middle() -> int {
    return leaf() * 2;
}

pure fn top() -> int {
    return middle() + 1;
}

fn main() {
    top();
}
