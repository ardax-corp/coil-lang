// Output: 1;-1;e
// `match f()? { … }` over a `Result<Option<int>, E>` (#1213).
use io::{stdout, write};
use string::{format, to_bytes};

fn get(int k) -> Result<Option<int>, string> {
    if k == 2 {
        raise "e";
    }
    if k == 1 {
        return Result::Ok(Option::None);
    }
    return Result::Ok(Option::Some(1));
}

fn f(int k) -> Result<int, string> {
    let n = match get(k)? {
        Option::Some(v) => v,
        Option::None => -1,
    };
    return n;
}

fn show(Result<int, string> r) -> string {
    return match r {
        Result::Ok(n) => format("%i", n),
        Result::Err(e) => e,
    };
}

fn main() {
    write(stdout(), to_bytes(format("%s;%s;%s", show(f(0)), show(f(1)), show(f(2)))));
}
