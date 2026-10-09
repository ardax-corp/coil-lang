// `Option<int>`, `Result<int, E>`, small payload enums and immediate pairs
// passed straight to a function travel as two words; fn values, partial
// application and deep tail recursion still see the same values.
enum Phase {
    Low(int),
    Mid(int),
    High(int),
}

fn score_opt(Option<int> o) -> int {
    return match o {
        Option::Some(v) => v,
        Option::None => -1,
    };
}

fn score_res(Result<int, string> r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(e) => -e.len(),
    };
}

fn score_phase(Phase p) -> int {
    return match p {
        Phase::Low(v) => v,
        Phase::Mid(x) => x + 100,
        Phase::High(y) => y + 200,
    };
}

fn mix(int a, Option<int> o, Result<int, string> r, int b) -> int {
    return a * 1000 + score_opt(o) * 100 + score_res(r) * 10 + b;
}

fn forward(Option<int> o) -> int {
    return score_opt(o);
}

fn keep(Option<int> o) -> Option<int> {
    return o;
}

fn store_it(Option<int> o) -> [Option<int>] {
    return [o, o];
}

fn count_down(Option<int> o, int n) -> int {
    if n == 0 {
        return score_opt(o);
    }
    return count_down(Option::Some(n), n - 1);
}

fn with_array(Option<int> o, int k) -> int {
    let xs = [1, 2, 3];
    let s = score_opt(o) + k;
    return s + xs[0] + xs.len();
}

fn add_opt(int a, Option<int> o) -> int {
    return a + score_opt(o);
}

fn apply(Option<int> -> int f, Option<int> o) -> int {
    return f(o);
}

fn reads_back(Option<int> o) -> int {
    let alias = o;
    let total = 0;
    if let Option::Some(v) = alias {
        total = v;
    }
    return total + score_opt(o);
}

test("option param") {
    assert(score_opt(Option::Some(5)) == 5)?;
    assert(score_opt(Option::None) == -1)?;
    let o = Option::Some(9);
    assert(score_opt(o) == 9)?;
}

test("result param") {
    assert(score_res(Result::Ok(4)) == 4)?;
    assert(score_res(Result::Err("abc")) == -3)?;
}

test("enum param") {
    assert(score_phase(Phase::Low(1)) == 1)?;
    assert(score_phase(Phase::Mid(1)) == 101)?;
    assert(score_phase(Phase::High(1)) == 201)?;
}

test("mixed params keep their order") {
    assert(mix(1, Option::Some(2), Result::Ok(3), 4) == 1234)?;
    assert(mix(1, Option::None, Result::Err("x"), 4) == 1000 - 100 - 10 + 4)?;
}

test("param forwarded, returned and stored") {
    assert(forward(Option::Some(3)) == 3)?;
    assert(score_opt(keep(Option::Some(6))) == 6)?;
    assert(score_opt(keep(Option::None)) == -1)?;
    let xs = store_it(Option::Some(8));
    assert(xs.len() == 2)?;
    assert(score_opt(xs[1]) == 8)?;
    assert(reads_back(Option::Some(4)) == 8)?;
}

test("deep tail recursion with an option param") {
    assert(count_down(Option::None, 300000) == 1)?;
}

test("call from a function with a stack array") {
    assert(with_array(Option::Some(10), 5) == 19)?;
}

test("partial application and fn values") {
    let f = add_opt(10);
    assert(f(Option::Some(1)) == 11)?;
    assert(f(Option::None) == 9)?;
    assert(apply(score_opt, Option::Some(7)) == 7)?;
}
