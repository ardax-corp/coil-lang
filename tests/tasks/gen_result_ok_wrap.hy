// A gen fn declared `-> Result<int, E>` whose `return` gives a bare int:
// the return is wrapped in Ok, and direct MIR lowers it too.
use tests::assert;

fn half(int n) -> Result<int, string> {
    if n % 2 != 0 {
        return Result::Err("odd");
    }
    return Result::Ok(n / 2);
}

gen fn quarter(int n) -> Result<int, string> {
    let h = half(n)?;
    let q = half(h)?;
    return q + 100;
}

fn ok_or(Result<int, string> r, int d) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => d,
    };
}

test("a gen fn returns a bare int as Ok") {
    let h = quarter(8);
    assert!(ok_or(resume h, -1) == 102);
    let odd = quarter(6);
    assert!(ok_or(resume odd, -1) == -1);
}
