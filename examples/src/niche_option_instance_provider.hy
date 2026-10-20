// Generic instances over `Option` / `Result` for another module to call
// at niche types (tests/generics/clone_niche_option_foreign_instance.hy,
// #1605).

trait Sz<T> {
    fn sz(T x) -> int {}
}

impl Sz for Option<T> {
    fn sz(Option<T> o) -> int {
        return match o {
            Some(_) => 1,
            None => 0,
        };
    }
}

impl Sz for Result<T, E> {
    fn sz(Result<T, E> r) -> int {
        return match r {
            Ok(_) => 1,
            Err(_) => 5,
        };
    }
}

trait Id<T> {
    fn id(T x) -> T {}
}

impl Id for Option<T> {
    fn id(Option<T> o) -> Option<T> {
        return o;
    }
}

trait Nm<T> {
    fn nm(T x) -> string {}
}

impl Nm for string {
    fn nm(string x) -> string {
        return x;
    }
}

impl Nm for Option<T: Nm> {
    fn nm(Option<T> o) -> string {
        return match o {
            Some(v) => "some " + v.nm(),
            None => "none",
        };
    }
}

trait Pick<T> {
    fn pick(T x, T y) -> T {}
}

impl Pick for Option<T> {
    fn pick(Option<T> a, Option<T> b) -> Option<T> {
        return match a {
            Some(_) => a,
            None => b,
        };
    }
}

fn lsize<T: Sz>(T x) -> int {
    return x.sz();
}

fn lkeep<T: Id>(T x) -> T {
    return x.id();
}
