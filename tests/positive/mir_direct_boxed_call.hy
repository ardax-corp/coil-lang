// A call whose `Option` comes back boxed (`Vec::pop`'s thunk) in bodies
// lowered straight from HIR to MIR: dropped as it comes back, or matched
// boxed with a `JumpIfMatch`.

fn drop_all(int n) -> int {
    let xs: Vec<int> = Vec::new();
    let i = 0;
    while i < n {
        xs.push(i);
        i = i + 1;
    }
    let _ = xs.pop();
    xs.pop();
    return xs.len();
}

fn drain(int n) -> int {
    let xs: Vec<int> = Vec::new();
    let i = 0;
    while i < n {
        xs.push(i * 2);
        i = i + 1;
    }
    let total = 0;
    let more = true;
    while more {
        match xs.pop() {
            Option::Some(x) => {
                total = total + x;
            },
            Option::None => {
                more = false;
            },
        }
    }
    return total;
}

test("boxed Option results in direct MIR") {
    assert(drop_all(5) == 3)?;
    assert(drop_all(1) == 0)?;
    assert(drain(4) == 12)?;
    assert(drain(0) == 0)?;
}
