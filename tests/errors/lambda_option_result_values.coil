// Lambdas (made in named functions) that return Option or Result values
// explicitly: the caller sees each variant, and `?` / `??` / `match` on the
// lambda's result work as on a named function's.
use tests::assert;

fn apply_opt(int -> Option<int> f, int x) -> int {
    return f(x) ?? -1;
}

fn apply_res(int -> Result<int, string> f, int x) -> string {
    return match f(x) {
        Result::Ok(v) => v.show(),
        Result::Err(e) => e,
    };
}

fn twice(int -> Result<int, string> f, int x) -> Result<int, string> {
    let a = f(x)?;
    let b = f(a)?;
    return b;
}

fn halves(int x) -> int {
    let half = fn (int n) {
        if n % 2 == 1 {
            return Option::None;
        }
        return Option::Some(n / 2);
    };
    return apply_opt(half, x);
}

fn checks(int x) -> string {
    let check = fn (int n) {
        if n < 0 {
            return Result::Err("neg");
        }
        return Result::Ok(n + 1);
    };
    return apply_res(check, x);
}

fn decrements(int x) -> Result<int, string> {
    let dec = fn (int n) {
        if n <= 0 {
            return Result::Err("spent");
        }
        return Result::Ok(n - 1);
    };
    return twice(dec, x);
}

test("an Option-returning lambda") {
    assert!(halves(8) == 4);
    assert!(halves(5) == -1);
}

test("a Result-returning lambda") {
    assert!(checks(1) == "2");
    assert!(checks(0) == "1");
    assert!(checks(-1) == "neg");
}

test("? on a lambda's result in a named function") {
    assert!(decrements(5) == Result::Ok(3));
    assert!(decrements(2) == Result::Ok(0));
    assert!(decrements(1) == Result::Err("spent"));
    assert!(decrements(0) == Result::Err("spent"));
}
