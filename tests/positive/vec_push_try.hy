use tests::assert;

fn finish() -> Result<string, string> {
    return "ok";
}

fn build() -> Result<Vec<string>, string> {
    let out = Vec::new();
    out.push(finish()?);
    return out;
}

test("push with nested try") {
    let r = build()?;
    assert!(r.len() == 1);
}
