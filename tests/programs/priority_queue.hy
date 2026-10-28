// A binary heap on a `Vec<T>` under an `Ord`-bounded inherent impl, used with ints and a derived-`Ord` class.
// @output [1, 2, 3, 5, 8, 9] peek=1 size=6 [(1, one), (2, two)]\n
use io::sync::println;
use string::format;

class Heap<T> {
    items: Vec<T>,
}

impl Heap<T: Ord> {
    pub static fn new() -> Heap<T> {
        return new Heap(Vec::new());
    }
    pub fn push(T x) {
        self.items.push(x);
        let i = self.items.len() - 1;
        while i > 0 {
            let p = (i - 1) / 2;
            if self.items[p] <= self.items[i] {
                break;
            }
            let t = self.items[p];
            self.items[p] = self.items[i];
            self.items[i] = t;
            i = p;
        }
    }
    pub fn pop() -> Option<T> {
        if self.items.len() == 0 {
            return Option::None;
        }
        let top = self.items[0];
        let last = self.items.pop() ?? top;
        if self.items.len() > 0 {
            self.items[0] = last;
            let i = 0;
            while true {
                let l = 2 * i + 1;
                let r = l + 1;
                let m = i;
                if l < self.items.len() && self.items[l] < self.items[m] {
                    m = l;
                }
                if r < self.items.len() && self.items[r] < self.items[m] {
                    m = r;
                }
                if m == i {
                    break;
                }
                let t = self.items[m];
                self.items[m] = self.items[i];
                self.items[i] = t;
                i = m;
            }
        }
        return Option::Some(top);
    }
    pub fn peek() -> Option<T> {
        if self.items.len() == 0 {
            return Option::None;
        }
        return Option::Some(self.items[0]);
    }
    pub fn len() -> int {
        return self.items.len();
    }
}

#[derive(Show, Eq, Ord)]
class Job {
    pub pri: int,
    pub name: string,
}

fn main() {
    let h = new Heap(Vec::from([5]));
    let _ = h.pop();
    for x in [5, 3, 8, 1, 9, 2] {
        h.push(x);
    }
    let peek = h.peek() ?? -1;
    let size = h.len();
    let out: Vec<int> = Vec::new();
    while let Option::Some(x) = h.pop() {
        out.push(x);
    }
    let jobs = new Heap(Vec::from([new Job(9, "nine")]));
    let _ = jobs.pop();
    jobs.push(new Job(2, "two"));
    jobs.push(new Job(1, "one"));
    let js: Vec<(int, string)> = Vec::new();
    while let Option::Some(j) = jobs.pop() {
        js.push((j.pri, j.name));
    }
    let _ = println(format("%v peek=%i size=%i %v", out, peek, size, js));
}
