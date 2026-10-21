// Output: 22,21,42,42
use io::{stdout, write};
use string::{format, to_bytes};

class BoxInt {
    pub opt: Option<int>,
}

enum Choice {
    A(int),
    B,
}

fn after_nested(BoxInt b) -> int {
    return match b.opt {
        Option::Some(v) => {
            let inner = match Option::Some(1) {
                Option::Some(x) => x,
                Option::None => 0,
            };
            v + inner
        },
        Option::None => 0,
    };
}

fn triple(BoxInt box) -> int {
    return match box.opt {
        Option::Some(a) => match box.opt {
            Option::Some(b) => match box.opt {
                Option::Some(c) => a + b + c,
                Option::None => -1,
            },
            Option::None => -2,
        },
        Option::None => 0,
    };
}

fn nested_result(Result<int, string> r) -> int {
    return match r {
        Result::Ok(v) => match r {
            Result::Ok(v2) => v + v2,
            Result::Err(_) => -1,
        },
        Result::Err(_) => 0,
    };
}

fn nested_choice(Choice c) -> int {
    return match c {
        Choice::A(x) => match c {
            Choice::A(y) => x + y,
            Choice::B => -1,
        },
        Choice::B => 0,
    };
}

fn main() {
    let b = new BoxInt(Option::Some(21));
    let t = new BoxInt(Option::Some(7));
    let _ = write(
        stdout(),
        to_bytes(
            format(
                "%i,%i,%i,%i",
                after_nested(b),
                triple(t),
                nested_result(Result::Ok(21)),
                nested_choice(Choice::A(21)),
            ),
        ),
    );
}
