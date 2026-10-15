// A statement `match` whose arms leave values of different shapes
// drops each arm's value: an arm whose block ends in an `if` that only
// returns falls through with nothing, beside an arm that leaves a word. The
// join must not try to merge the two (it once refused with "arm value").
use tests::assert;

enum Dir {
    Left,
    Right,
}

fn pick(Result<string, string> r) -> string {
    match r {
        Result::Ok(s) => {
            if s.len() > 0 {
                return s;
            }
        },
        Result::Err(_) => 0,
    }
    return "none";
}

fn first_some(Option<int> o) -> int {
    match o {
        Option::Some(v) => {
            if v > 0 {
                return v;
            }
        },
        Option::None => 0,
    }
    return -1;
}

fn by_int(int n) -> int {
    match n {
        0 => 5,
        default => {
            if n > 10 {
                return n;
            }
        },
    }
    return 1;
}

fn by_dir(Dir d, int n) -> int {
    match d {
        Dir::Left => {
            if n > 0 {
                return n;
            }
        },
        Dir::Right => 3,
    }
    return 0;
}

test("Result statement match: returning arm beside an int arm") {
    assert!(pick(Result::Ok("a")) == "a");
    assert!(pick(Result::Ok("")) == "none");
    assert!(pick(Result::Err("e")) == "none");
}

test("Option statement match") {
    assert!(first_some(Option::Some(4)) == 4);
    assert!(first_some(Option::Some(-2)) == -1);
    assert!(first_some(Option::None) == -1);
}

test("int statement match") {
    assert!(by_int(0) == 1);
    assert!(by_int(3) == 1);
    assert!(by_int(12) == 12);
}

test("enum statement match") {
    assert!(by_dir(Dir::Left, 2) == 2);
    assert!(by_dir(Dir::Left, 0) == 0);
    assert!(by_dir(Dir::Right, 2) == 0);
}
