// An operator on a class with an instance (`Eq for P`, `Add for V`) lowers to
// a CALL that stages its operands in temp slots. As an operand of `&&` /
// `+` it must be staged itself, or those temps overwrite the other side's
// live result: `(a == b) && (c == d)` was false for equal pairs, which broke
// every derived `Eq` / `Ord` whose fields have instances.

class V {
    pub x: int,
}

impl Add for V {
    pub fn add(V a, V b) -> V {
        return new V(a.x + b.x);
    }
}

#[derive(Eq)]
class P {
    pub x: int,
}

fn mk(int x) -> P {
    return new P(x);
}

fn both(P a, P b, P c, P d) -> bool {
    return (a == b) && (c == d);
}

test("instance == on both sides of &&") {
    assert(both(new P(1), new P(1), new P(2), new P(2)))?;
    assert(!both(new P(1), new P(1), new P(2), new P(3)))?;
}

test("instance + inside another instance +") {
    let v = (new V(1) + new V(2)) + (new V(3) + new V(4));
    assert(v.x == 10)?;
}

test("parenthesized calls as operands") {
    assert((mk(1) == mk(1)) && (mk(2) == mk(2)))?;
}
