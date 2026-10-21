// #1700: `use prelude::Option as Opt;` names the built-in `Option`, so the
// alias works as a type, a constructor head and a pattern head like the
// original name; the same goes for `Result` and `io`'s `IoError`.
use tests::assert;
use prelude::{Option as Opt, Result as Res};
use io::IoError as IoErr;

fn first(Opt<int> o) -> int {
    return o ?? 0;
}

fn wrap<T>(T x) -> Opt<T> {
    return Opt::Some(x);
}

fn half(int n) -> Res<int, string> {
    if n % 2 == 0 {
        return Res::Ok(n / 2);
    }
    return Res::Err("odd");
}

fn missing(IoErr e) -> bool {
    return match e {
        IoErr::NotFound => true,
        default => false,
    };
}

test("an aliased Option is a type and constructor head") {
    assert!(first(Opt::Some(4)) == 4);
    assert!(first(Opt::None) == 0);
    let o: Opt<string> = wrap("hi");
    assert!((o ?? "none") == "hi");
    let d = match Opt::Some(7) {
        Opt::Some(v) => v,
        Opt::None => 0,
    };
    assert!(d == 7);
}

test("an aliased Result is a type, constructor and pattern head") {
    let c = match half(6) {
        Res::Ok(v) => v,
        Res::Err(_) => 0,
    };
    assert!(c == 3);
    assert!((half(3) ?? -1) == -1);
}

test("an aliased IoError is a type and pattern head") {
    assert!(missing(IoErr::NotFound));
    assert!(!missing(IoErr::TimedOut));
}
