// Strict `&&` / `||` over pure compares lower to bool BitAnd / BitOr in MIR.
fn fizz(int n) -> int {
    let c = 0;
    let i = 0;
    while i < n {
        if i % 3 == 0 || i % 5 == 0 {
            c = c + 1;
        }
        if i > 10 && i < 20 {
            c = c + 100;
        }
        i = i + 1;
    }
    return c;
}

test("bool or / and in a counted loop") {
    assert(fizz(100) == 947)?;
    assert(fizz(0) == 0)?;
}
