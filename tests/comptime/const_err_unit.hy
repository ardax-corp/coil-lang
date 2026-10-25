// A const holding `Err` of a unit variant (or of `()`) keeps it an `Err`.
use tests::assert;

enum Why {
    Missing,
    Broken,
}

fn find(int n) -> Result<string, Why> {
    if n > 0 {
        return Result::Ok("found");
    }
    return Result::Err(Why::Broken);
}

fn check(int n) -> Result<string, ()> {
    if n > 0 {
        return Result::Ok("fine");
    }
    return Result::Err(());
}

const LOST = find(0);

const FOUND = find(1);

const FAILED = check(0);

test("Err of a unit variant") {
    let why = match LOST {
        Result::Ok(_) => "ok",
        Result::Err(Why::Missing) => "missing",
        Result::Err(Why::Broken) => "broken",
    };
    assert!(why == "broken");
    assert!((FOUND.ok() ?? "") == "found");
}

test("Err of ()") {
    let failed = match FAILED {
        Result::Ok(_) => false,
        Result::Err(_) => true,
    };
    assert!(failed);
}
