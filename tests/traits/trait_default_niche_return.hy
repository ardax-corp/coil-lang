// A trait default method may return a niche `Option` / `Result` (a heap
// payload): the default body returns it in the layout its callers take.
use tests::assert;

class Span {
    pub start: int,
    pub end: int,
}

trait Find<P> {
    fn find_in(P pat, string hay) -> Option<Span> {}
}

impl Find for string {
    pub fn find_in(string pat, string hay) -> Option<Span> {
        let at = string::find_from(hay, pat, 0);
        if at < 0 {
            return None;
        }
        return Some(new Span(at, at + pat.len()));
    }
}

trait Hay<T: Show> {
    fn first(T hay, string pat) -> Option<Span> {
        let at = string::find_from(hay.show(), pat, 0);
        if at < 0 {
            return None;
        }
        return Some(new Span(at, at + pat.len()));
    }

    fn word(T hay) -> Option<string> {
        return Some(hay.show());
    }

    fn checked(T hay) -> Result<string, string> {
        let s = hay.show();
        if s.len() == 0 {
            raise "empty";
        }
        return s;
    }

    fn first_of<P: Find>(T hay, P pat) -> Option<Span> {
        return pat.find_in(hay.show());
    }
}

impl Hay for string {
}

impl Hay for int {
}

test("default methods return niche Option and Result") {
    let s = "foobar".first("ob") ?? new Span(-1, -1);
    assert!(s.start == 2 && s.end == 4);
    assert!("foobar".first("zz").is_none());
    assert!(("ab".word() ?? "") == "ab");
    assert!((12.word() ?? "") == "12");
    assert!(("ab".checked() ?? "") == "ab");
    assert!("".checked().is_err());
}

test("a default forwards a bound call's niche Option") {
    let s = "foobar".first_of("bar") ?? new Span(-1, -1);
    assert!(s.start == 3 && s.end == 6);
    assert!(345.first_of("9").is_none());
}
