// A bounded generic function in another module: a caller at a type this
// module does not know reaches its shared body with trait dictionaries
// (examples/perf/dict_once.hy).

fn larger<T: Ord>(T a, T b) -> T {
    if a > b {
        return a;
    }
    return b;
}

// A bounded generic function calling another: a caller's ground call gets
// per-type clones of both, compiled here (examples/perf/foreign_fn_clone.hy).
fn largest<T: Ord>(Vec<T> xs, T start) -> T {
    let best = start;
    for x in xs {
        best = larger(best, x);
    }
    return best;
}

// A bounded generic class in another module: a caller's ground calls get
// per-type clones compiled here (examples/perf/foreign_clone.hy).
class Tally<K> {
    keys: Vec<K>,
    counts: Vec<int>,
}

impl Tally<K> {
    pub static fn new() -> Tally<K> {
        return new Tally(Vec::new(), Vec::new());
    }
}

impl Tally<K: Eq> {
    fn find(K k) -> int {
        let i = 0;
        while i < self.keys.len() {
            if self.keys[i] == k {
                return i;
            }
            i = i + 1;
        }
        return 0 - 1;
    }

    pub fn add(K k) {
        let at = self.find(k);
        if at < 0 {
            self.keys.push(k);
            self.counts.push(1);
        } else {
            self.counts[at] = self.counts[at] + 1;
        }
    }

    pub fn count(K k) -> int {
        let at = self.find(k);
        if at < 0 {
            return 0;
        }
        return self.counts[at];
    }
}
