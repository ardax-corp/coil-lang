use tests::assert;

fn finish() -> Result<string, string> {
    return "ok";
}

fn build() -> Result<Vec<string>, string> {
    let out = Vec::new();
    let pkg = finish()?;
    out.push(pkg);
    return out;
}

test("push with temp try") {
    let r = build()?;
    assert!(r.len() == 1);
}
