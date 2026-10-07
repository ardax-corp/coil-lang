// Coroutine bodies lower through HIR: statement and receiving `yield`,
// `yield from`, `?` inside an async fn, and `block_on`.

async fn count(int n) {
    let i = 0;
    while i < n {
        yield i;
        i = i + 1;
    }
}

async fn echo(int start) {
    let got = yield start;
    let again = yield got + 1;
    yield again * 2;
}

async fn twice() {
    yield from count(2);
    yield from count(2);
}

fn parse(int n) -> Result<int, string> {
    if n < 0 {
        return Result::Err("negative");
    }
    return Result::Ok(n * 10);
}

async fn checked(int n) -> Result<int, string> {
    let v = parse(n)?;
    yield Result::Ok(v);
    return Result::Ok(v + 1);
}

async fn greet() -> int {
    yield 1;
    return 2;
}

test("statement yield") {
    let h = count(3);
    let a = resume h;
    let b = resume h;
    let c = resume h;
    assert(a + b + c == 3)?;
}

test("receiving yield") {
    let e = echo(5);
    let x = resume e;
    let y = resume e with 10;
    let z = resume e with 7;
    assert(x == 5)?;
    assert(y == 11)?;
    assert(z == 14)?;
}

test("yield from") {
    let h = twice();
    let a = resume h;
    let b = resume h;
    assert(a == 0)?;
    assert(b == 1)?;
}

test("question mark in an async fn") {
    let bad = checked(-1);
    let first = match resume bad {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
    assert(first == "negative")?;
    let good = checked(2);
    let v = match resume good {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
    assert(v == 20)?;
}

test("block_on") {
    assert(block_on(greet()) == 2)?;
}
