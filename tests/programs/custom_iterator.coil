// A user class with `impl Iterator` / `impl IntoIterator` driven by `for`, next to a method that collects a stepped range.
// @output 1 2 4 8 16 | 5 | 3 6 9 | fizz buzz fizzbuzz\n
use io::sync::println;
use string::format;

class Powers {
    pub cur: int,
    pub limit: int,
}

impl Iterator for Powers {
    type Item = int;
    fn next(Powers it) -> Option<int> {
        if it.cur > it.limit {
            return Option::None;
        }
        let v = it.cur;
        it.cur = it.cur * 2;
        return Option::Some(v);
    }
}

impl IntoIterator for Powers {
    type Item = int;
    type IntoIter = Powers;
    fn into_iter(Powers p) -> Powers {
        return p;
    }
}

class Step {
    pub from: int,
    pub to: int,
    pub by: int,
}

impl Step {
    pub fn collect() -> Vec<int> {
        let out: Vec<int> = Vec::new();
        let i = self.from;
        while i < self.to {
            out.push(i);
            i += self.by;
        }
        return out;
    }
}

fn fizz(int n) -> string {
    return match (n % 3 == 0, n % 5 == 0) {
        default => if n % 15 == 0 {
            "fizzbuzz"
        } else if n % 3 == 0 {
            "fizz"
        } else if n % 5 == 0 {
            "buzz"
        } else {
            format("%i", n)
        },
    };
}

fn main() {
    let s = "";
    for p in new Powers(1, 16) {
        s += format("%i ", p);
    }
    let count = 0;
    for _ in new Powers(1, 16) {
        count += 1;
    }
    let t = "";
    for x in new Step(3, 10, 3).collect() {
        t += format("%i ", x);
    }
    let _ = println(format("%s| %i | %s| %s %s %s", s, count, t, fizz(3), fizz(5), fizz(15)));
}
