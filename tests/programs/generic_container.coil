// A generic queue class and a memo table used by a memoised fibonacci, with `if let` over the lookup.
// @output 3 Some(2) None [10, 20, 30] 60 true cache-hit=2\n
use io::sync::println;
use string::format;

class Queue<T> {
    items: Vec<T>,
    head: int,
}

impl Queue<T> {
    pub static fn new() -> Queue<T> {
        return new Queue(Vec::new(), 0);
    }
    pub fn enqueue(T x) {
        self.items.push(x);
    }
    pub fn dequeue() -> Option<T> {
        if self.head >= self.items.len() {
            return Option::None;
        }
        let v = self.items[self.head];
        self.head += 1;
        return Option::Some(v);
    }
    pub fn len() -> int {
        return self.items.len() - self.head;
    }
    pub fn is_empty() -> bool {
        return self.len() == 0;
    }
}

class Memo {
    keys: Vec<int>,
    vals: Vec<int>,
    hits: int,
}

impl Memo {
    pub static fn new() -> Memo {
        return new Memo(Vec::new(), Vec::new(), 0);
    }
    pub fn get(int k) -> Option<int> {
        let i = 0;
        while i < self.keys.len() {
            if self.keys[i] == k {
                self.hits += 1;
                return Option::Some(self.vals[i]);
            }
            i += 1;
        }
        return Option::None;
    }
    pub fn put(int k, int v) {
        self.keys.push(k);
        self.vals.push(v);
    }
    pub fn hits() -> int {
        return self.hits;
    }
}

fn fib_memo(int n, Memo m) -> int {
    if n < 2 {
        return n;
    }
    if let Option::Some(v) = m.get(n) {
        return v;
    }
    let v = fib_memo(n - 1, m) + fib_memo(n - 2, m);
    m.put(n, v);
    return v;
}

fn main() {
    let q: Queue<int> = Queue::new();
    q.enqueue(1);
    q.enqueue(2);
    q.enqueue(3);
    let n = q.len();
    let _ = q.dequeue();
    let second = q.dequeue();
    let _ = q.dequeue();
    let empty = q.dequeue();
    let qs: Queue<string> = Queue::new();
    qs.enqueue("a");
    let m = Memo::new();
    let f = fib_memo(5, m);
    let tens = Vec::from([10, 20, 30]);
    let tot = 0;
    for t in tens {
        tot += t;
    }
    let _ = println(
        format(
            "%i %v %v %v %i %z cache-hit=%i",
            n,
            second,
            empty,
            tens,
            tot,
            q.is_empty() && !qs.is_empty(),
            m.hits() - (f - 5),
        ),
    );
}
