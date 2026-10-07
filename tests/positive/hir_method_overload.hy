// Overloaded inherent methods by arity, including calls from one
// overload to another inside the impl.
class Tally {
    pub n: int,
    pub log: string,
}

impl Tally {
    pub fn add(int by) -> int {
        self.n = self.n + by;
        return self.n;
    }

    pub fn add() -> int {
        return self.add(1);
    }

    pub fn add(int a, int b) -> int {
        return self.add(a * b);
    }

    pub fn note(string s) -> string {
        self.log = self.log + s;
        return self.log;
    }

    pub fn note(string a, string b) -> string {
        self.note(a);
        return self.note(b);
    }
}

test("overloads by arity") {
    let t = new Tally(0, "");
    assert(t.add() == 1)?;
    assert(t.add(4) == 5)?;
    assert(t.add(2, 3) == 11)?;
}

test("string overloads") {
    let t = new Tally(0, "");
    assert(t.note("a") == "a")?;
    assert(t.note("b", "c") == "abc")?;
}
