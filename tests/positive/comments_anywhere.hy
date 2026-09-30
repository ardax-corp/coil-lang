// Comments are trivia: `//` and `/* */` (nestable) may sit between any
// two tokens, not only in statement position.

/* A block comment
   spanning lines /* with a nested one */ still closes here. */
enum Shape {
    // leading variant comment
    Dot, // trailing variant comment
    Line(int), /* after a payload */
}

class Pair {
    // leading field comment
    pub a: int, // trailing field comment
    /* inline */
    pub b: int,
}

impl Pair {
    // leading method comment
    pub fn sum() -> int { // after the brace
        return self.a + self.b;
        /* mid-expression */
    }
}

fn measure(Shape s) -> int {
    return match s {
        // leading arm comment
        Shape::Dot => 0, // trailing arm comment
        Shape::Line(n) => {
            n
        },
        /* after an arm */
    };
}

test("comments between list and record items") {
    let xs = [
        1, // one
        // before two
        2,
        /* three */
        3,
    ];
    let d = {
        a: 1, /* between */
        b: 2,
    };
    assert(xs[0] + xs[1] + xs[2] == 6)?;
    assert(d.a + d.b == 3)?;
}

test("comments inside calls, classes and matches") {
    let p = new Pair(
        /* a */
        2, // then b
        3,
    );
    assert(p.sum() == 5)?;
    assert(
        measure(
            Shape::Line(7),
            /* n */
        ) == 7,
    )?;
    assert(measure(Shape::Dot) == 0)?;
}

test("comment-like text in strings stays text") {
    let s = "http://example.com /* not a comment */";
    assert(s.len() == 38)?;
}
