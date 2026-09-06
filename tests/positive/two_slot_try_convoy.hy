// Two-slot `?` flatten (#307) next to arity-2 product ABI (#302):
// fail tags must not steal later args; products must not leak into Result tags.

fn step(int n) -> Result<int, int> {
    if n < 0 {
        return Result::Err(n);
    }
    return Result::Ok(n);
}

fn coords(int n) -> (int, int) {
    return (n, n + 1);
}

fn httpish(int n) -> Result<int, int> {
    let x = step(n)?;
    let (a, b) = coords(x);
    let y = step(a + b)?;
    return y;
}

fn maybe(int n) -> Option<int> {
    if n == 0 {
        return Option::None;
    }
    return Option::Some(n);
}

fn option_pipe(int n) -> Option<int> {
    let a = maybe(n)?;
    let b = maybe(a - 1)?;
    return maybe(b);
}

fn header(int code) -> Result<int, int> {
    if code == 404 {
        return Result::Err(-404);
    }
    if code == 408 {
        return Result::Err(-408);
    }
    return Result::Ok(code);
}

fn body(int n) -> Result<int, int> {
    if n == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(n / 2);
}

fn request(int code) -> Result<int, int> {
    let h = header(code)?;
    let b = body(h)?;
    let extra = step(b)?;
    return extra + 1;
}

test("try then product then try keeps both ABIs") {
    assert(match httpish(4) {
        Result::Ok(v) => v == 9,
        Result::Err(_) => false,
    })?;
    assert(match httpish(-2) {
        Result::Ok(_) => false,
        Result::Err(e) => e == -2,
    })?;
}

test("option question chain flatten") {
    assert(match option_pipe(3) {
        Option::Some(v) => v == 2,
        Option::None => false,
    })?;
    assert(match option_pipe(1) {
        Option::Some(_) => false,
        Option::None => true,
    })?;
    assert(match option_pipe(0) {
        Option::Some(_) => false,
        Option::None => true,
    })?;
}

test("http-shaped three-step result chain") {
    assert(match request(200) {
        Result::Ok(v) => v == 101,
        Result::Err(_) => false,
    })?;
    assert(match request(404) {
        Result::Ok(_) => false,
        Result::Err(e) => e == -404,
    })?;
    assert(match request(0) {
        Result::Ok(_) => false,
        Result::Err(e) => e == -1,
    })?;
}
