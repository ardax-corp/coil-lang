// A match whose first arm matches anything (a bare name or `default`) takes
// the scrutinee as it is, whatever its type: a `float`, a `string`, a class,
// a tuple, or the `Ok` payload of `?` in a function returning `Result`
// (#1658).
use tests::assert;

class Pt {
    pub x: int,
    pub y: int,
}

fn inner(int x) -> Result<string, int> {
    if x > 0 {
        return Result::Ok("abc");
    }
    return Result::Err(x);
}

fn len_of_ok(int y) -> Result<int, int> {
    let r = match inner(y)? {
        s => s.len(),
    };
    return Result::Ok(r);
}

fn ret_match(int y) -> Result<int, int> {
    return match inner(y)? {
        s => Result::Ok(s.len() + 1),
    };
}

fn bump(float y) -> float {
    return match y {
        x => x + 1.0,
    };
}

fn sum(Pt p) -> int {
    return match p {
        q => q.x + q.y,
    };
}

fn pair_sum((int, int) t) -> int {
    let (a, b) = match t {
        q => q,
    };
    return a + b;
}

fn ignore(string s) -> int {
    return match s {
        default => 7,
    };
}

fn stmt(float y) -> float {
    let acc = 0.0;
    match y * 2.0 {
        x => {
            acc = acc + x;
        },
    }
    return acc;
}

test("a binding arm on the Ok payload of `?`") {
    assert!(
        match len_of_ok(1) {
            Result::Ok(n) => n == 3,
            Result::Err(_) => false,
        },
    );
    assert!(
        match len_of_ok(-4) {
            Result::Ok(_) => false,
            Result::Err(e) => e == -4,
        },
    );
    assert!(
        match ret_match(1) {
            Result::Ok(n) => n == 4,
            Result::Err(_) => false,
        },
    );
}

test("a binding arm on a float, a class and a tuple") {
    assert!(bump(1.5) == 2.5);
    assert!(sum(new Pt(2, 3)) == 5);
    assert!(pair_sum((4, 5)) == 9);
}

test("a default arm and a statement match") {
    assert!(ignore("x") == 7);
    assert!(stmt(1.5) == 3.0);
}
