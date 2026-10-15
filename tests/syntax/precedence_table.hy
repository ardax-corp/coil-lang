// The operator precedence table, high to low: postfix, prefix unary, `as`,
// `**`, `* / %`, `+ -`, shifts, `&`, `^`, `|`, comparisons, ranges,
// equality, `&&`, `||`, `??`, assignment.
use tests::assert;

fn neg_one() -> int {
    return -1;
}

test("prefix minus binds tighter than **") {
    assert!(-2 ** 2 == 4);
    assert!(-(2 ** 2) == -4);
    assert!(-2 ** 3 == -8);
    let x = 3;
    assert!(-x ** 2 == 9);
}

test("cast sits between unary and **") {
    let n = 2;
    assert!(n as float ** 3.0 == 8.0);
    assert!(-n as float == -2.0);
    assert!(n as float * 2.5 == 5.0);
}

test("bitwise below additive, comparisons below bitwise") {
    assert!(6 & 3 == 2);
    assert!(1 | 2 ^ 3 == 1);
    assert!((1 | 2) ^ 3 == 0);
    assert!(5 & 4 + 1 == 5);
    assert!(1 + 2 << 1 == 6);
}

test("comparison binds tighter than equality") {
    assert!(1 < 2 == 3 < 4);
    assert!(2 > 1 != 1 > 2);
    assert!(!(1 < 2 == 3 > 4));
}

test("&& binds tighter than ||") {
    assert!(true || false && false);
    assert!(!(false && true || false));
    assert!(false || true && true);
    assert!((true || false) && true);
}

test("! binds tighter than == and &&") {
    assert!(!false == true);
    assert!(!true == false);
    assert!(!false && true);
}

test("ranges sit between comparison and equality") {
    let r = 1 + 1..2 * 3;
    let n = 0;
    for x in r {
        n = n + x;
    }
    assert!(n == 2 + 3 + 4 + 5);
}

test("?? binds below || and above assignment") {
    let a: Option<int> = Option::None;
    let b: Option<int> = Option::Some(4);
    let v = a ?? 1 + 2;
    assert!(v == 3);
    let w = 0;
    w = b ?? 9;
    assert!(w == 4);
}

test("postfix and call bind tightest") {
    let xs = [10, 20, 30];
    assert!(-xs[1] == -20);
    assert!(-neg_one() == 1);
    let t = ((1, 2), 3);
    assert!(t[0][1] * 2 == 4);
}
