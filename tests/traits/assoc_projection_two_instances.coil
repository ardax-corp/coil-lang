// A generic using `C::Elem` is called at two instances whose `Elem`
// differs; each call gets its own instance's type (#1187).
// @output 7 x 0 y 2 1\n
use io::{stdout, write};
use string::{format, to_bytes};

trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem {}
    fn count(C xs) -> int {}
}

class Words {
    pub w: string,
}

impl Collect for Option<int> {
    type Elem = int;

    pub fn head(Option<int> xs) -> int {
        return xs ?? 0;
    }

    pub fn count(Option<int> xs) -> int {
        return match xs {
            Option::Some(_) => 1,
            Option::None => 0,
        };
    }
}

impl Collect for Words {
    type Elem = string;

    pub fn head(Words w) -> string {
        return w.w;
    }

    pub fn count(Words w) -> int {
        return 2;
    }
}

fn or_head<C: Collect>(C xs, C::Elem fallback) -> C::Elem {
    if count(xs) == 0 {
        return fallback;
    }
    return head(xs);
}

fn take_head<C: Collect>(C xs) -> C::Elem {
    return head(xs);
}

fn main() {
    let a = or_head(Option::Some(7), 5);
    let b = or_head(new Words("x"), "z");
    let c = take_head(Option::None);
    let d = take_head(new Words("y"));
    let n = count(new Words("w"));
    let m = count(Option::Some(3));
    let _ = write(stdout(), to_bytes(format("%i %s %i %s %i %i\n", a, b, c, d, n, m)));
}
