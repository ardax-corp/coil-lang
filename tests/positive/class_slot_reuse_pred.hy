// Mirrors coil-http H2ClientSlot reuse predicate: free fn over class fields.
// DenseFieldLoad of a string field in a callee must match LoadField in the caller.
class Slot {
    pub on: int,
    pub key: string,
    pub next: int,
    pub sess: Option<int>,
}

impl Slot {
    pub fn key_eq_m(string key) -> int {
        if self.key == key {
            return 1;
        }
        return 0;
    }
}

fn can_reuse(Slot slot, string key) -> int {
    if slot.on == 0 {
        return 0;
    }
    if slot.key != key {
        return 2;
    }
    if slot.next < 1 {
        return 3;
    }
    if slot.next % 2 == 0 {
        return 4;
    }
    let _s = match slot.sess {
        Option::None => {
            return 5;
        },
        Option::Some(v) => v,
    };
    return 1;
}

fn key_eq(Slot slot, string key) -> int {
    if slot.key == key {
        return 1;
    }
    return 0;
}

fn key_neq_then_zero(Slot slot, string key) -> int {
    if slot.key != key {
        return 0;
    }
    return 1;
}

test("reuse predicate after field stores") {
    let slot = new Slot(0, "", 1, Option::None);
    slot.on = 1;
    slot.key = "http://127.0.0.1:9";
    slot.next = 3;
    slot.sess = Option::Some(1);
    let key = "http://127.0.0.1:9";
    let g = can_reuse(slot, key);
    if g == 2 {
        panic "key";
    }
    if g == 3 {
        panic "next_lt";
    }
    if g == 4 {
        panic "next_even";
    }
    if g == 5 {
        panic "sess";
    }
    if g == 0 {
        panic "on";
    }
    assert(g == 1)?;
}

test("string field equals arg in caller") {
    let slot = new Slot(0, "", 1, Option::None);
    slot.key = "http://127.0.0.1:9";
    let key = "http://127.0.0.1:9";
    assert(slot.key == key)?;
}

test("string field equals arg in callee") {
    let slot = new Slot(0, "", 1, Option::None);
    slot.key = "http://127.0.0.1:9";
    let key = "http://127.0.0.1:9";
    assert(key_eq(slot, key) == 1)?;
}

test("string field not-equal inverted in callee") {
    let slot = new Slot(0, "", 1, Option::None);
    slot.key = "http://127.0.0.1:9";
    let key = "http://127.0.0.1:9";
    assert(key_neq_then_zero(slot, key) == 1)?;
}

test("string field equals arg on method") {
    let slot = new Slot(0, "", 1, Option::None);
    slot.key = "http://127.0.0.1:9";
    let key = "http://127.0.0.1:9";
    assert(slot.key_eq_m(key) == 1)?;
}

test("reuse predicate rejects empty slot") {
    let slot = new Slot(0, "", 1, Option::None);
    let key = "http://127.0.0.1:9";
    assert(can_reuse(slot, key) == 0)?;
}
