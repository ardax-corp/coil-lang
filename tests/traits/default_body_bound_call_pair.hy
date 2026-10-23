// A trait default body reads a two-word result of a bound method call (a
// product tuple, a range) from the box the method's dictionary entry
// returns (#1859).
use tests::assert;

trait Pattern<P> {
    fn find_in(P pat, string hay, int from) -> (int, int) {}
    fn span_in(P pat, string hay) -> Range<int> {}
}

impl Pattern for string {
    pub fn find_in(string pat, string hay, int from) -> (int, int) {
        let at = string::find_from(hay, pat, from);
        if at < 0 {
            return (-1, -1);
        }
        return (at, at + pat.len());
    }

    pub fn span_in(string pat, string hay) -> Range<int> {
        let at = string::find_from(hay, pat, 0);
        return at..at + pat.len();
    }
}

trait Haystack<T: Show> {
    fn has<P: Pattern>(T hay, P pat) -> bool {
        let (s, _) = pat.find_in(hay.show(), 0);
        return s >= 0;
    }

    fn end_of<P: Pattern>(T hay, P pat) -> int {
        let (_, e) = pat.find_in(hay.show(), 0);
        return e;
    }

    fn span_len<P: Pattern>(T hay, P pat) -> int {
        let n = 0;
        for _ in pat.span_in(hay.show()) {
            n += 1;
        }
        return n;
    }
}

impl Haystack for string {
}

test("default body destructures a bound call's tuple") {
    assert!("foobar".has("oba"));
    assert!(!"foobar".has("xyz"));
    assert!("foobar".end_of("oba") == 5);
}

test("default body reads a bound call's range") {
    assert!("foobar".span_len("ob") == 2);
}
