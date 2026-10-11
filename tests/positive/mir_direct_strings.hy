// Strings in bodies lowered straight from HIR to MIR: a string is a heap
// word, and a literal loads from the string table.
fn label(int code) -> string {
    if code == 200 {
        return "ok";
    }
    if code == 404 {
        return "missing";
    }
    return "error";
}

fn pick(string a, string b, bool first) -> string {
    if first {
        return a;
    }
    return b;
}

fn count_ok(int n) -> int {
    let hits = 0;
    let i = 0;
    while i < n {
        let s = label(200 + i % 3);
        if len(s) == 2 {
            hits = hits + 1;
        }
        i = i + 1;
    }
    return hits;
}

test("strings in direct MIR") {
    assert(label(200) == "ok")?;
    assert(label(404) == "missing")?;
    assert(label(500) == "error")?;
    assert(pick("a", "b", false) == "b")?;
    assert(count_ok(9) == 3)?;
}
