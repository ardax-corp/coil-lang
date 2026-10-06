// `s[i]` lowers from HIR: the byte at `i`, also above live operands.
fn first(string s) -> string {
    return s;
}

fn spread(string s) -> int {
    return (s[len(s) - 1] as int) - (s[0] as int);
}

test("index under an operand") {
    let s = "az";
    let d = (s[1] as int) - (s[0] as int);
    assert(d == 25)?;
    assert((first(s)[1] as int) - (first(s)[0] as int) == 25)?;
}

test("index in a call argument") {
    assert(spread("abcz") == 25)?;
    assert(spread("q") == 0)?;
}

test("index beside other args") {
    let s = "hello";
    let sum = (s[0] as int) + (s[1] as int) + (s[4] as int);
    assert(sum == 104 + 101 + 111)?;
}
