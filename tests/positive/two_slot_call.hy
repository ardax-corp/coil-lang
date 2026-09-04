// Direct CALL/RETURN two-slot ABI for known ≤2-word return layouts.
//
// `Result<int, int>`, `Result<int, heap-object>` (including unit-enum
// errors), `Option<int>`, and user payload enums of arity ≤1 move
// `[payload, tag]` on a direct CALL/RETURN instead of boxing an `ObjEnum`.
// Matching the call directly (`match f(...) { ... }`) uses the same
// EQ/JMPF lowering as a frame-local match (#278) — no allocation. `?`
// and `let r = f()` stay on the pair; box only at escape. Niched
// heap `Option<T>` / heap-heap `Result<T,E>` stay one word (unaffected).

enum HttpError {
    NotFound,
    Timeout(string),
}

fn checked_div(int a, int b) -> Result<int, int> {
    if b == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(a / b);
}

fn maybe(int n) -> Option<int> {
    if n < 0 {
        return Option::None;
    }
    return Option::Some(n);
}

fn fetch(int code) -> Result<int, HttpError> {
    if code == 404 {
        return Result::Err(HttpError::NotFound);
    }
    if code == 408 {
        return Result::Err(HttpError::Timeout("slow"));
    }
    return Result::Ok(code);
}

enum Cell {
    Num(int),
    Empty,
}

// Payload variant declared *after* the unit variant — the two-slot
// classifier and its boxing cascade must not assume tag 0 is the
// payload-carrying variant.
enum CellRev {
    Empty,
    Num(int),
}

fn cell(int n) -> Cell {
    if n < 0 {
        return Cell::Empty;
    }
    return Cell::Num(n);
}

fn cell_rev(int n) -> CellRev {
    if n < 0 {
        return CellRev::Empty;
    }
    return CellRev::Num(n);
}

fn chained(int a, int b) -> Result<int, int> {
    let q = checked_div(a, b)?;
    return q + 1;
}

test("result int int direct match") {
    assert(match checked_div(10, 2) {
        Result::Ok(q) => q == 5,
        Result::Err(_) => false,
    })?;
    assert(match checked_div(1, 0) {
        Result::Ok(_) => false,
        Result::Err(e) => e == -1,
    })?;
}

test("option int direct match") {
    assert(match maybe(3) {
        Option::Some(v) => v == 3,
        Option::None => false,
    })?;
    assert(match maybe(-1) {
        Option::Some(_) => false,
        Option::None => true,
    })?;
}

test("result int heap unit-enum error direct match") {
    assert(match fetch(200) {
        Result::Ok(code) => code == 200,
        Result::Err(_) => false,
    })?;
    assert(match fetch(404) {
        Result::Ok(_) => false,
        Result::Err(e) => match e {
            HttpError::NotFound => true,
            HttpError::Timeout(_) => false,
        },
    })?;
    assert(match fetch(408) {
        Result::Ok(_) => false,
        Result::Err(e) => match e {
            HttpError::NotFound => false,
            HttpError::Timeout(msg) => msg == "slow",
        },
    })?;
}

test("payload enum arity 1 direct match, either variant order") {
    assert(match cell(4) {
        Cell::Num(v) => v == 4,
        Cell::Empty => false,
    })?;
    assert(match cell(-2) {
        Cell::Num(_) => false,
        Cell::Empty => true,
    })?;
    assert(match cell_rev(4) {
        CellRev::Num(v) => v == 4,
        CellRev::Empty => false,
    })?;
    assert(match cell_rev(-2) {
        CellRev::Num(_) => false,
        CellRev::Empty => true,
    })?;
}

test("bind site keeps two-slot local (match / ? without boxing)") {
    let x = maybe(2);
    assert(match x {
        Option::Some(v) => v == 2,
        Option::None => false,
    })?;
    let r = checked_div(9, 3);
    assert(match r {
        Result::Ok(v) => v == 3,
        Result::Err(_) => false,
    })?;
    let q = checked_div(8, 2)?;
    assert(q == 4)?;
}

test("two-word Result Try propagates through raise/?") {
    assert(match chained(9, 3) {
        Result::Ok(v) => v == 4,
        Result::Err(_) => false,
    })?;
    assert(match chained(1, 0) {
        Result::Ok(_) => false,
        Result::Err(e) => e == -1,
    })?;
}

test("niched heap Option/Result stay one word (unaffected)") {
    let some_text = Option::Some("ok");
    assert(match some_text {
        Option::Some(s) => s == "ok",
        Option::None => false,
    })?;
    let ok_pair: Result<string, string> = Result::Ok("fine");
    assert(match ok_pair {
        Result::Ok(s) => s == "fine",
        Result::Err(_) => false,
    })?;
}
