// HIR only: coil-lang#785 (the AST walk evaluates a bound `<` after a false `&&` left side).
// A bound type parameter's operators in a generic class's shared method
// body dispatch through the hidden dictionary, for every key type.
class Sorted<K> {
    pub items: Vec<K>,
}

impl Sorted<K: Ord + Eq> {
    pub fn add(K k) -> bool {
        for x in self.items {
            if x == k {
                return false;
            }
        }
        // Push, then sink the new key below every larger one.
        self.items.push(k);
        let i = self.items.len() - 1;
        while i > 0 && self.items[i] < self.items[i - 1] {
            let t = self.items[i - 1];
            self.items[i - 1] = self.items[i];
            self.items[i] = t;
            i -= 1;
        }
        return true;
    }

    /// How many keys sort below `k`.
    pub fn rank(K k) -> int {
        let n = 0;
        for x in self.items {
            if x < k {
                n += 1;
            }
        }
        return n;
    }

    pub fn has(K k) -> bool {
        for x in self.items {
            if x == k {
                return true;
            }
        }
        return false;
    }
}

test("bound operators in shared bodies") {
    let a = new Sorted(Vec::new());
    assert(a.add(3))?;
    assert(a.add(-7))?;
    assert(a.add(5))?;
    assert(a.add(3) == false)?;
    assert(a.rank(-7) == 0)?;
    assert(a.rank(4) == 2)?;
    assert(a.rank(9) == 3)?;
    assert(a.has(5))?;
    assert(a.has(4) == false)?;

    let b = new Sorted(Vec::new());
    assert(b.add(2.5))?;
    assert(b.add(-1.25))?;
    assert(b.add(-3.5))?;
    assert(b.add(-1.25) == false)?;
    assert(b.rank(-2.0) == 1)?;
    assert(b.rank(0.0) == 2)?;
    assert(b.has(2.5))?;
    assert(b.has(0.5) == false)?;
}
