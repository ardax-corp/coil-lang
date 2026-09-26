// A file's own free function shadows the implicit prelude one (`diff`).
fn diff(int n) -> int {
    if n <= 1 {
        return n;
    }
    return diff(n - 1) - diff(n - 2);
}

test("user fn shadows prelude diff") {
    assert(diff(10) == -1)?;
}
