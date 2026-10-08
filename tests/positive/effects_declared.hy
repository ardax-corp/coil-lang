// E3: `pure fn` and `uses {…}` hold, a trait method's declaration gives
// calls through the trait its effects, and a declaration says nothing
// about the functions a higher-order function is passed.
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

static let HITS: int = 0;

trait Area<A> {
    pure fn area(A self) -> int {}
}

class Sq {
    pub side: int,
}

class Rect {
    pub w: int,
    pub h: int,
}

impl Area for Sq {
    fn area(Sq self) -> int {
        return self.side * self.side;
    }
}

impl Area for Rect {
    fn area(Rect self) -> int {
        return self.w * self.h;
    }
}

pure fn total(Area a, Area b) -> int {
    return area(a) + area(b);
}

pure fn nth([int; 3] v, int i) -> int {
    assert(i >= 0);
    return v[i];
}

pure fn bytes_of(string s) -> int {
    return len(to_bytes(s));
}

fn apply(int -> int f, int x) -> int uses {} {
    return f(x);
}

fn bump(int x) -> int uses {read, mutate} {
    HITS = HITS + x;
    return HITS;
}

fn say(string s) uses {write, suspend} {
    write_all(stdout(), to_bytes(s));
}

test("declared functions run as written") {
    assert(total(new Sq(2), new Rect(2, 3)) == 10)?;
    assert(nth([4, 5, 6], 1) == 5)?;
    assert(bytes_of("abc") == 3)?;
    assert(apply(fn (int x) => x + 1, 1) == 2)?;
    assert(apply(fn (int x) => bump(x), 2) == 2)?;
    assert(bump(3) == 5)?;
    say("");
}
