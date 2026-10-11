// String `+` in bodies lowered straight from HIR to MIR: a two-argument
// format over both sides.
fn greet(string name) -> string {
    return "hello, " + name + "!";
}

fn repeat(string s, int n) -> string {
    let out = "";
    let i = 0;
    while i < n {
        out = out + s;
        i = i + 1;
    }
    return out;
}

test("string concat in direct MIR") {
    assert(greet("coil") == "hello, coil!")?;
    assert(repeat("ab", 3) == "ababab")?;
    assert(len(repeat("x", 0)) == 0)?;
}
