// `Result::Ok(())` in a `Result<unit, E>` context is the niche unit-result
// word (Ok = 0), whether the make is returned from a generic class method,
// returned from a plain function, or inlined into a caller's `match`. The
// make's own type is `Result<(), E>`, so the word must follow the context.
use tests::assert;

enum Err2 {
    Closed,
    Full,
}

class Box2<T> {
    pub buf: Vec<Option<T>>,
    pub len: int,
    pub closed: bool,
}

impl Box2<T> {
    pub fn put(T value) -> Result<unit, Err2> {
        if self.closed {
            return Result::Err(Err2::Closed);
        }
        let cap = self.buf.len();
        if self.len == cap {
            return Result::Err(Err2::Full);
        }
        self.buf[self.len % cap] = Option::Some(value);
        self.len = self.len + 1;
        return Result::Ok(());
    }
}

fn plain(bool c) -> Result<unit, Err2> {
    if c {
        return Result::Err(Err2::Closed);
    }
    return Result::Ok(());
}

test("generic unit result") {
    let v: Vec<Option<int>> = Vec::new();
    v.push(Option::None);
    let b = new Box2(v, 0, false);
    let r = b.put(1);
    assert!(match r {
        Result::Ok(_) => true,
        default => false,
    });
    let r2 = b.put(2);
    assert!(match r2 {
        Result::Err(Err2::Full) => true,
        default => false,
    });
    assert!(
        match plain(true) {
            Result::Err(Err2::Closed) => true,
            default => false,
        },
    );
    assert!(match plain(false) {
        Result::Ok(_) => true,
        default => false,
    });
}
