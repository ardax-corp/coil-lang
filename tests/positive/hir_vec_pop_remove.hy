// `Vec::pop` / `Vec::remove` returning a niche `Option` call the host
// natives directly: the native id, the receiver and index, `HostInvoke`.
fn drain_sum(Vec<int> v) -> int {
    let acc = 0;
    let more = true;
    while more {
        match v.pop() {
            Option::Some(x) => {
                acc = acc + x;
            }
            Option::None => {
                more = false;
            }
        }
    }
    return acc;
}

fn take_at(Vec<string> v, int i) -> string {
    let got = v.remove(i);
    return match got {
        Option::Some(s) => s,
        Option::None => "none",
    };
}

fn drop_last(Vec<int> v) -> int {
    v.pop();
    return v.len();
}

test("pop drains a vec") {
    let v = Vec::from([1, 2, 3, 4]);
    assert(drain_sum(v) == 10)?;
    assert(v.len() == 0)?;
}

test("remove by index") {
    let v = Vec::from(["a", "b", "c"]);
    assert(take_at(v, 1) == "b")?;
    assert(take_at(v, 5) == "none")?;
    assert(v.len() == 2)?;
}

test("discarded pop") {
    let v = Vec::from([7, 8]);
    assert(drop_last(v) == 1)?;
    assert(drop_last(v) == 0)?;
    assert(drop_last(v) == 0)?;
}
