// Match arms that meet in one value lower to direct MIR when one arm
// yields an `Option` payload of a `Result` and another builds the
// `Option` itself (`Option::None`).
use tests::assert;

enum Fault {
    WouldBlock,
    Other,
}

fn fetch(int k) -> Result<Option<int>, Fault> {
    if k == 0 {
        return Result::Err(Fault::WouldBlock);
    }
    if k == 1 {
        return Result::Err(Fault::Other);
    }
    return Result::Ok(Option::Some(k));
}

fn settle(int k) -> Result<int, Fault> {
    if k > 5 {
        return Result::Err(Fault::Other);
    }
    return Result::Ok(k);
}

fn or_none(int k) -> int {
    let got = match fetch(k) {
        Result::Ok(o) => o,
        Result::Err(_) => Option::None,
    };
    return match got {
        Option::Some(n) => n,
        Option::None => -1,
    };
}

fn parked(int k) -> int {
    let waited = 0;
    let got = match fetch(k) {
        Result::Ok(o) => o,
        Result::Err(Fault::WouldBlock) => {
            match settle(k) {
                Result::Ok(_) => {
                    waited = 1;
                    Option::None
                },
                Result::Err(_) => panic "settle",
            }
        },
        Result::Err(_) => Option::Some(100),
    };
    return match got {
        Option::Some(n) => n + waited,
        Option::None => waited * 10,
    };
}

test("arms meet in one Option") {
    assert!(or_none(0) == -1);
    assert!(or_none(1) == -1);
    assert!(or_none(7) == 7);
    assert!(parked(0) == 10);
    assert!(parked(1) == 100);
    assert!(parked(4) == 4);
}
