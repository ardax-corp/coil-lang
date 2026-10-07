// Named, spread and rest call arguments become positional: fixed ones in
// parameter order, then the rest packed into one array argument.
use string::format;

fn diff(int a, int b) -> int {
    return a - b;
}

fn sum(int... xs) -> int {
    let total = 0;
    let i = 0;
    while i < len(xs) {
        total = total + xs[i];
        i = i + 1;
    }
    return total;
}

fn label(string head, int... xs) -> string {
    let total = 0;
    for x in xs {
        total = total + x;
    }
    return head + format("%i", total);
}

test("named arguments take their parameter's place") {
    assert(diff(a: 10, b: 2) == 8)?;
    assert(diff(10, b: 3) == 7)?;
}

test("rest arguments pack into one array") {
    assert(sum() == 0)?;
    assert(sum(4) == 4)?;
    assert(sum(1, 2, 3) == 6)?;
    assert(label("n", 1, 2) == "n3")?;
    assert(label(head: "m") == "m0")?;
}

test("spread literals and tuples fill parameters") {
    assert(diff(...(9, 4)) == 5)?;
    assert(sum(...[1, 2, 3, 4]) == 10)?;
    let t = (20, 5);
    assert(diff(...t) == 15)?;
}
