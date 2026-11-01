// A match whose nested arms together cover every payload needs no
// `default`: Result of Option, a two-slot tuple payload, and a user enum
// nested in an Option.
use tests::assert;

enum Color {
    Red,
    Green,
}

enum Pair {
    Two(Option<int>, Option<int>),
}

fn result_option(Result<Option<int>, string> r) -> int {
    return match r {
        Result::Ok(Option::Some(v)) => v,
        Result::Ok(Option::None) => 0,
        Result::Err(_) => -1,
    };
}

fn pair(Pair p) -> int {
    return match p {
        Pair::Two(Option::Some(a), Option::Some(b)) => a + b,
        Pair::Two(Option::Some(a), Option::None) => a,
        Pair::Two(Option::None, Option::Some(b)) => -b,
        Pair::Two(Option::None, Option::None) => 0,
    };
}

fn color(Option<Color> c) -> string {
    return match c {
        Option::Some(Color::Red) => "red",
        Option::Some(Color::Green) => "green",
        Option::None => "none",
    };
}

test("Result of Option") {
    assert!(result_option(Result::Ok(Option::Some(5))) == 5);
    assert!(result_option(Result::Ok(Option::None)) == 0);
    assert!(result_option(Result::Err("e")) == -1);
}

test("every combination of a two-slot payload") {
    assert!(pair(Pair::Two(Option::Some(2), Option::Some(3))) == 5);
    assert!(pair(Pair::Two(Option::Some(2), Option::None)) == 2);
    assert!(pair(Pair::Two(Option::None, Option::Some(3))) == -3);
    assert!(pair(Pair::Two(Option::None, Option::None)) == 0);
}

test("user enum nested in Option") {
    assert!(color(Option::Some(Color::Red)) == "red");
    assert!(color(Option::Some(Color::Green)) == "green");
    assert!(color(Option::None) == "none");
}
