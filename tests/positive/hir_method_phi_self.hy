// A result-mode method whose `?` assigns join in a φ read once by the
// final call: the φ keeps its own slot instead of reusing `self`'s.

class Reader {
    pub base: int,
}

impl Reader {
    fn small(int at) -> Result<int, string> {
        if at < 0 {
            return Result::Err("negative");
        }
        return self.base + at;
    }

    fn wide(int k, int at) -> Result<int, string> {
        return self.base * k + at;
    }

    fn sized(int n, int at) -> Result<int, string> {
        return self.base + n * 100 + at;
    }

    pub fn pick(int b, int at) -> Result<int, string> {
        let n = 0;
        if b == 1 {
            n = self.small(at)?;
        } else {
            if b == 2 {
                n = self.wide(2, at)?;
            } else {
                n = self.wide(4, at)?;
            }
        }
        return self.sized(n, at)?;
    }
}

fn got(Result<int, string> r) -> int {
    match r {
        Result::Ok(v) => {
            return v;
        },
        Result::Err(_) => {
            return -1;
        },
    }
}

test("a joined local beside self keeps its own slot") {
    let r = new Reader(1);
    assert(got(r.pick(1, 3)) == 1 + 400 + 3)?;
    assert(got(r.pick(2, 3)) == 1 + 500 + 3)?;
    assert(got(r.pick(3, 3)) == 1 + 700 + 3)?;
}
