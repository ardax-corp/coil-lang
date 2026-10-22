// @expected E0209 — `Two(None, Some(_))` is covered by no arm.
enum Pair {
    Two(Option<int>, Option<int>),
}

fn f(Pair p) -> int {
    return match p {
        Pair::Two(Option::Some(a), _) => a,
        Pair::Two(Option::None, Option::None) => 0,
    };
}

fn main() {
    let r = f(Pair::Two(Option::None, Option::None));
}
