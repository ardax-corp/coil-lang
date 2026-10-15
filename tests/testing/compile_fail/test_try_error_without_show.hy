// Expected: E0114 — `?` in a test needs an error type with `Show`.
fn risky() -> Result<int, Vec<int>> {
    raise Vec::new();
}

test("Vec<int> has no Show") {
    let x = risky()?;
}
