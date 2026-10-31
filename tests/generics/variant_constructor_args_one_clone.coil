// A generic called with a variant constructor (`Option::Some(1)`), a typed
// `Option::None` and an `Option<int>` value runs one clone at
// `Option<int>`, and each call reads its own value (#1477).
use tests::assert;

enum Pick<T> {
    One(T),
    Two(T, T),
    Zero,
}

enum Rank {
    Lo,
    Hi,
}

fn id<T>(T x) -> T {
    return x;
}

fn second<T>(T a, T b) -> T {
    return b;
}

fn or_zero(Option<int> o) -> int {
    return o ?? 0;
}

fn pick_sum(Pick<int> p) -> int {
    return match p {
        Pick::One(a) => a,
        Pick::Two(a, b) => a + b,
        Pick::Zero => 0,
    };
}

test("Option constructors and a typed value share one clone") {
    let x: Option<int> = Option::Some(2);
    assert!(or_zero(id(Option::Some(1))) == 1);
    let n: Option<int> = id(Option::None);
    assert!(or_zero(n) == 0);
    assert!(or_zero(id(x)) == 2);
}

test("mixed variants bind one type parameter") {
    assert!(or_zero(second(Option::Some(5), Option::None)) == 0);
    assert!(or_zero(second(Option::None, Option::Some(5))) == 5);
    let r: Result<int, string> = second(Result::Err("no"), Result::Ok(3));
    assert!((r.ok() ?? 0) == 3);
}

test("user enum constructors share their enum's clone") {
    let p: Pick<int> = Pick::Two(2, 3);
    assert!(pick_sum(id(Pick::One(4))) == 4);
    let z: Pick<int> = id(Pick::Zero);
    assert!(pick_sum(z) == 0);
    assert!(pick_sum(id(p)) == 5);
    assert!(second(Rank::Lo, Rank::Hi) == Rank::Hi);
}
