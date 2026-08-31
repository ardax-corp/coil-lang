// Vec method surface: pop/insert/remove/clear/reserve/capacity/from + rest packs.

fn sum3([int; 3] xs) -> int {
    return xs[0] + xs[1] + xs[2];
}

fn rest_len(int... xs) -> int {
    return xs.len();
}

fn rest_push_len(int... xs) -> int {
    xs.push(99);
    return xs.len();
}

test("pop some and none") {
    let v = Vec::from([1, 2]);
    let a = match v.pop() {
        Option::Some(n) => n,
        Option::None => -1,
    };
    assert(a == 2)?;
    assert(v.len() == 1)?;
    let _ = v.pop();
    let empty = match v.pop() {
        Option::Some(_) => false,
        Option::None => true,
    };
    assert(empty)?;
}

test("insert mid") {
    let v = Vec::from([1, 3]);
    v.insert(1, 2);
    assert(v.len() == 3)?;
    assert(v[0] == 1)?;
    assert(v[1] == 2)?;
    assert(v[2] == 3)?;
}

test("remove some and oob none") {
    let v = Vec::from([10, 20, 30]);
    let mid = match v.remove(1) {
        Option::Some(n) => n,
        Option::None => -1,
    };
    assert(mid == 20)?;
    assert(v.len() == 2)?;
    assert(v[0] == 10)?;
    assert(v[1] == 30)?;

    let miss = match v.remove(5) {
        Option::Some(_) => false,
        Option::None => true,
    };
    assert(miss)?;
    let neg = match v.remove(-1) {
        Option::Some(_) => false,
        Option::None => true,
    };
    assert(neg)?;
}

test("clear keeps capacity floor") {
    let v = Vec::with_capacity(8);
    assert(v.capacity() >= 8)?;
    v.push(1);
    v.push(2);
    v.clear();
    assert(v.len() == 0)?;
    assert(v.capacity() >= 8)?;
}

test("reserve grows capacity") {
    let v: Vec<int> = Vec::new();
    v.reserve(32);
    assert(v.capacity() >= 32)?;
    assert(v.len() == 0)?;
}

test("from copies independently") {
    let fixed = [1, 2, 3];
    let v = Vec::from(fixed);
    v[0] = 9;
    assert(fixed[0] == 1)?;
    assert(v[0] == 9)?;
    assert(v.len() == 3)?;
}

test("fixed local escapes to callee") {
    let a = [4, 5, 6];
    a[1] = 50;
    assert(sum3(a) == 60)?;
    assert(a[1] == 50)?;
}

test("empty fixed zero") {
    let z: [int; 0] = [];
    assert(len(z) == 0)?;
}

test("rest pack is vec") {
    assert(rest_len(1, 2, 3) == 3)?;
    assert(rest_len() == 0)?;
    assert(rest_push_len(1, 2) == 3)?;
}
