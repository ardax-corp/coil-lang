// C2b rung 1: heap-field and array-held Range counted for-in.

class Holder {
    pub r: Range<int>,
}

class HolderInc {
    pub r: RangeInclusive<int>,
}

fn sum_field(Holder h) -> int {
    let acc = 0;
    for x in h.r {
        acc = acc + x;
    }
    return acc;
}

fn sum_array([Range<int>] rs) -> int {
    let acc = 0;
    for x in rs[0] {
        acc = acc + x;
    }
    return acc;
}

fn sum_inc_field(HolderInc h) -> int {
    let acc = 0;
    for x in h.r {
        acc = acc + x;
    }
    return acc;
}

test("heap-field range for-in") {
    assert(sum_field(new Holder(0..5)) == 10)?;
}

test("array-held range for-in") {
    assert(sum_array([0..5]) == 10)?;
}

test("heap-field inclusive range for-in") {
    assert(sum_inc_field(new HolderInc(0..=4)) == 10)?;
}
