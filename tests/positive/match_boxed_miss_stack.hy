// A `match` on a boxed enum tests each arm with `JumpIfMatch`, which leaves
// the scrutinee on the operand stack when it misses. An arm reached that way
// must take it off again even when it unpacks no payload (`None => 1`, a
// later arm's test), or a loop running the match grows the stack each time.
use tests::assert;

enum Shape {
    Dot,
    Line(int),
    Box(int, int),
}

class Job {
    pub limit: Option<int>,
    pub shape: Shape,
}

fn limit_sum(Job j, int n) -> int {
    let t = 0;
    let i = 0;
    while i < n {
        let l = match j.limit {
            Option::Some(v) => v,
            Option::None => 1,
        };
        t = t + l;
        i = i + 1;
    }
    return t;
}

fn shape_sum(Job j, int n) -> int {
    let t = 0;
    let i = 0;
    while i < n {
        let s = match j.shape {
            Shape::Line(a) => a,
            Shape::Box(w, h) => w * h,
            Shape::Dot => 1,
        };
        t = t + s;
        i = i + 1;
    }
    return t;
}

test("a unit last arm reached by a miss leaves the stack as it was") {
    let j = new Job(None, Shape::Dot);
    assert!(limit_sum(j, 200000) == 200000);
    assert!(shape_sum(j, 200000) == 200000);
    j.limit = Some(2);
    j.shape = Shape::Box(2, 3);
    assert!(limit_sum(j, 200000) == 400000);
    assert!(shape_sum(j, 200000) == 1200000);
}
