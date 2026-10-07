// A field read through a dynamic index into a small array literal selects
// over its frame slots; under a binary operator the left operand stages
// through a temp first, as the AST does.

class N {
    pub v: int,
}

test("field of a selected element") {
    let nodes = [new N(1), new N(2), new N(3)];
    let total = 0;
    let i = 0;
    while i < 3 {
        total = total + nodes[i].v;
        i = i + 1;
    }
    assert(total == 6)?;
}

test("select in a comparison") {
    let nodes = [new N(4), new N(9)];
    let i = 1;
    assert(5 < nodes[i].v)?;
    assert(nodes[0].v * 2 + nodes[i].v == 17)?;
}
