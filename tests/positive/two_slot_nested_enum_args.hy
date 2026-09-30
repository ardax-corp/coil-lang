// Enum values built inside a nested call's arguments keep their own layout
// when the enclosing expression is lowered in a special representation:
// a two-slot `[payload, tag]` pair (two-word return, match scrutinee,
// unboxed bind, `?`) or a pointer niche (niche match scrutinee, `??`).

class Taker {
    pub bias: int,
}

impl Taker {
    pub fn take(Result<int, string> r) -> int {
        return self.bias + rlen(r);
    }
}

fn rlen(Result<int, string> r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(e) => 1000 + e.len(),
    };
}

fn oget(Option<int> o) -> int {
    return match o {
        Option::Some(v) => v,
        Option::None => -1,
    };
}

fn opt_of(int n) -> Option<int> {
    return Option::Some(n);
}

fn rid(Result<int, string> r) -> Result<int, string> {
    return Result::Ok(rlen(r));
}

fn err_text(Result<int, string> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
}

fn oid(Option<int> o) -> Option<int> {
    return Option::Some(oget(o) + 1);
}

enum Pick {
    Num(int),
    Name(string),
}

fn pick_len(Pick p) -> Pick {
    return match p {
        Pick::Num(n) => Pick::Num(n),
        Pick::Name(s) => Pick::Num(s.len()),
    };
}

fn pick_num(Pick p) -> int {
    return match p {
        Pick::Num(n) => n,
        Pick::Name(_) => -1,
    };
}

fn pair_of(Option<int> o) -> (int, int) {
    return (oget(o), 1);
}

// --- two-word return: fast `Construct` whose payload is a call ---

fn ret_ok_of_call_result_arg() -> Result<int, string> {
    return Result::Ok(rlen(Result::Err("boom")));
}

fn ret_ok_of_call_option_arg() -> Result<int, string> {
    return Result::Ok(oget(Option::Some(42)));
}

fn ret_ok_of_call_two_word_call_arg() -> Result<int, string> {
    return Result::Ok(oget(opt_of(7)));
}

fn ret_ok_of_method_arg() -> Result<int, string> {
    let t = new Taker(5);
    return Result::Ok(t.take(Result::Err("boom")));
}

fn ret_ok_of_binop_call_arg() -> Result<int, string> {
    return Result::Ok(1 + rlen(Result::Err("boom")));
}

fn ret_err_of_call_arg() -> Result<int, string> {
    return Result::Err(err_text(Result::Err("inner")));
}

fn ret_some_nested() -> Option<int> {
    return Option::Some(oget(Option::Some(oget(Option::Some(2)))));
}

fn ret_some_of_call_local_arg() -> Option<int> {
    let o = Option::Some(5);
    return Option::Some(oget(o));
}

// --- two-word return: fast pass-through `CALL` ---

fn ret_call_option_arg() -> Option<int> {
    return oid(Option::Some(3));
}

fn ret_call_two_word_call_arg() -> Option<int> {
    return oid(oid(Option::Some(3)));
}

fn ret_call_user_enum_arg() -> Pick {
    return pick_len(Pick::Name("abcdef"));
}

fn ret_product_call_arg() -> (int, int) {
    return pair_of(Option::Some(4));
}

// --- match scrutinee on a two-word call ---

fn scrutinee_call_arg() -> int {
    return match rid(Result::Err("boom")) {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
}

fn scrutinee_call_arg_in_result_fn() -> Result<int, string> {
    let n = match rid(Result::Err("boom")) {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
    return Result::Ok(n);
}

// --- unboxed local binds / rebinds ---

fn bind_construct_of_call() -> int {
    let x = Option::Some(oget(Option::Some(3)));
    return oget(x);
}

fn bind_two_word_call() -> int {
    let x = rid(Result::Err("boom"));
    return rlen(x);
}

fn bind_match_arm_call(int k) -> int {
    let x = match k {
        0 => Option::Some(oget(Option::Some(9))),
        default => Option::None,
    };
    return oget(x);
}

fn bind_match_arm_block(int k) -> int {
    let x = match k {
        0 => {
            let t = oget(Option::Some(1));
            rlen(Result::Err("zz"));
            Option::Some(t + 20)
        },
        default => Option::None,
    };
    return oget(x);
}

fn rebind_construct_of_call() -> int {
    let x = Option::Some(0);
    x = Option::Some(oget(Option::Some(11)));
    return oget(x);
}

// --- `?` on a two-word call ---

fn try_call_arg() -> Result<int, string> {
    let v = rid(Result::Err("boom"))?;
    return Result::Ok(v);
}

fn try_return_call_arg() -> Result<int, string> {
    return Result::Ok(rid(Result::Err("boom"))?);
}

// Option<string> / Result<string, string>: pointer-niche returns.
fn name_of(Option<int> o) -> Option<string> {
    if oget(o) > 0 {
        return Option::Some("pos");
    }
    return Option::None;
}

fn describe(Result<int, string> r) -> Result<string, string> {
    if rlen(r) > 100 {
        return Result::Err("err");
    }
    return Result::Ok("ok");
}

fn niche_opt_scrutinee() -> int {
    return match name_of(Option::Some(3)) {
        Option::Some(s) => s.len(),
        Option::None => 0,
    };
}

fn niche_result_scrutinee() -> int {
    return match describe(Result::Err("boom")) {
        Result::Ok(s) => s.len(),
        Result::Err(e) => 10 + e.len(),
    };
}

fn niche_arms(Option<string> o) -> Option<string> {
    return match o {
        Option::Some(s) => Option::Some(s + "!"),
        default => Option::None,
    };
}

// --- two-word return: payload is itself a construct / instance ---

fn ret_err_of_user_enum() -> Result<int, Pick> {
    return Result::Err(Pick::Name("bad"));
}

fn ret_some_of_some() -> Option<Option<int>> {
    return Option::Some(Option::Some(8));
}

fn ret_ok_of_instance() -> Result<Taker, string> {
    return Result::Ok(new Taker(12));
}

fn unwrap(Result<int, string> r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(e) => -1000 - e.len(),
    };
}

test("return Ok(call(Err(..)))") {
    assert(unwrap(ret_ok_of_call_result_arg()) == 1004)?;
}

test("return Ok(call(Some(..)))") {
    assert(unwrap(ret_ok_of_call_option_arg()) == 42)?;
}

test("return Ok(call(two-word call))") {
    assert(unwrap(ret_ok_of_call_two_word_call_arg()) == 7)?;
}

test("return Ok(method(Err(..)))") {
    assert(unwrap(ret_ok_of_method_arg()) == 1009)?;
}

test("return Ok(n + call(Err(..)))") {
    assert(unwrap(ret_ok_of_binop_call_arg()) == 1005)?;
}

test("return Err(call(Err(..)))") {
    assert(err_text(ret_err_of_call_arg()) == "inner")?;
}

test("return Some(call(Some(call(Some(..)))))") {
    assert(oget(ret_some_nested()) == 2)?;
}

test("return Some(call(local))") {
    assert(oget(ret_some_of_call_local_arg()) == 5)?;
}

test("return call(Some(..))") {
    assert(oget(ret_call_option_arg()) == 4)?;
}

test("return call(call(Some(..)))") {
    assert(oget(ret_call_two_word_call_arg()) == 5)?;
}

test("return call(user enum ctor)") {
    assert(pick_num(ret_call_user_enum_arg()) == 6)?;
}

test("return product call(Some(..))") {
    let (a, b) = ret_product_call_arg();
    assert(a == 4 && b == 1)?;
}

test("match call(Err(..))") {
    assert(scrutinee_call_arg() == 1004)?;
}

test("match call(Err(..)) in Result fn") {
    assert(unwrap(scrutinee_call_arg_in_result_fn()) == 1004)?;
}

test("match call(Err(..)) in test body") {
    let n = match rid(Result::Err("boom")) {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
    assert(n == 1004)?;
}

test("let x = Some(call(Some(..)))") {
    assert(bind_construct_of_call() == 3)?;
}

test("let x = call(Err(..))") {
    assert(bind_two_word_call() == 1004)?;
}

test("let x = match { _ => Some(call(Some(..))) }") {
    assert(bind_match_arm_call(0) == 9)?;
}

test("let x = match { _ => { stmts; Some(..) } }") {
    assert(bind_match_arm_block(0) == 21)?;
}

test("x = Some(call(Some(..)))") {
    assert(rebind_construct_of_call() == 11)?;
}

test("call(Err(..))?") {
    assert(unwrap(try_call_arg()) == 1004)?;
}

test("return Ok(call(Err(..))?)") {
    assert(unwrap(try_return_call_arg()) == 1004)?;
}

test("match niche call(Some(..))") {
    assert(niche_opt_scrutinee() == 3)?;
}

test("match niche call(Err(..))") {
    assert(niche_result_scrutinee() == 13)?;
}

test("call(Some(..)) ?? default") {
    assert((name_of(Option::Some(3)) ?? "none") == "pos")?;
}

test("call(Err(..)) ?? default") {
    assert((describe(Result::Err("boom")) ?? "fallback") == "fallback")?;
}

test("niche match arms rebuild Some") {
    assert((niche_arms(Option::Some("hi")) ?? "none") == "hi!")?;
}

test("return Err(UserEnum::V(..))") {
    let n = match ret_err_of_user_enum() {
        Result::Ok(_) => -1,
        Result::Err(p) => match p {
            Pick::Num(_) => -2,
            Pick::Name(s) => s.len(),
        },
    };
    assert(n == 3)?;
}

test("return Some(Some(..))") {
    let n = match ret_some_of_some() {
        Option::Some(inner) => oget(inner),
        Option::None => -2,
    };
    assert(n == 8)?;
}

test("return Ok(new C(..))") {
    let n = match ret_ok_of_instance() {
        Result::Ok(t) => t.bias,
        Result::Err(_) => -1,
    };
    assert(n == 12)?;
}
