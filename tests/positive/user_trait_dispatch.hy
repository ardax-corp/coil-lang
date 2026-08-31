// COI-78: user-trait method at a ground type vs the same trait under a
// generic bound must agree on the result. Dispatch differs (CALL vs
// dictionary CallIndirect) but the ABI is one dictionary convention.

trait Measurable<T> {
    fn size(T x) -> int;
}

impl Measurable<int> {
    pub fn size(int x) -> int {
        return x + 1;
    }
}

fn size_of<T: Measurable>(T x) -> int {
    return x.size();
}

fn size_of_ufcs<T: Measurable>(T x) -> int {
    return size(x);
}

// Second method exercises Index slot selection under an open bound.
trait PairOps<T> {
    fn left(T x) -> int;
    fn right(T x) -> int;
}

impl PairOps<int> {
    pub fn left(int x) -> int {
        return x;
    }
    pub fn right(int x) -> int {
        return x + 10;
    }
}

fn pair_right_of<T: PairOps>(T x) -> int {
    return x.right();
}

fn len_of<T: Length>(T x) -> int {
    return len(x);
}

test("user trait at a ground type") {
    assert(41.size() == 42)?;
}

test("user trait under a generic bound") {
    assert(size_of(41) == 42)?;
    assert(size_of_ufcs(41) == 42)?;
}

test("multi-method user trait via dictionary") {
    assert(pair_right_of(3) == 13)?;
}

test("Length bound stays on dictionary ABI") {
    assert(len_of("ab") == 2)?;
}
