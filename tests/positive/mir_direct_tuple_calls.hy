// A tuple that does not travel as a two-word pair is a heap tuple: a call
// returns it boxed and takes it boxed, in direct MIR as in HIR emission.
fn triple(int i) -> (int, int, int) {
    return (i, i * 2, i * 3);
}

fn sum3((int, int, int) t) -> int {
    return t[0] + t[1] + t[2];
}

fn tagged(int n) -> (int, string) {
    return (n, "n");
}

fn label((int, string) p) -> string {
    if p[0] > 5 {
        return p[1];
    }
    return p[1] + p[1];
}

test("heap tuples through calls") {
    let t = triple(2);
    assert(t[0] + t[1] + t[2] == 12)?;
    assert(sum3(triple(1)) == 6)?;
    assert(sum3(t) == 12)?;
    let p = tagged(4);
    assert(label(p) == "nn")?;
    assert(label(tagged(7)) == "n")?;
}
