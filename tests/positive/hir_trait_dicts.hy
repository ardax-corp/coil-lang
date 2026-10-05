// Calls to bounded generics from HIR-lowered bodies: bare type-parameter
// arguments are boxed, one dictionary per constraint follows the
// arguments, and a bare type-parameter result is unboxed.

trait Weigh<T> {
    fn weight(T x) -> int {}
}

impl Weigh for int {
    pub fn weight(int x) -> int {
        return x * 2;
    }
}

impl Weigh for string {
    pub fn weight(string x) -> int {
        return len(x);
    }
}

fn heavier<T: Weigh>(T a, T b) -> T {
    if a.weight() >= b.weight() {
        return a;
    }
    return b;
}

fn total<T: Weigh>(Vec<T> xs) -> int {
    let sum = 0;
    let i = 0;
    while i < len(xs) {
        sum = sum + xs[i].weight();
        i = i + 1;
    }
    return sum;
}

class Bag<T> {
    items: Vec<T>,
}

impl Bag<T: Weigh> {
    pub fn add(T v) -> int {
        self.items.push(v);
        return v.weight();
    }

    pub fn first_or(T fallback) -> T {
        if len(self.items) == 0 {
            return fallback;
        }
        return self.items[0];
    }
}

fn pick(int a, int b) -> int {
    let xs: Vec<int> = [];
    xs.push(a);
    xs.push(b);
    return heavier(a, b) + total(xs);
}

fn longest(string a, string b) -> string {
    return heavier(a, b) + "!";
}

fn fill(Bag<string> bag) -> int {
    let w = bag.add("abc");
    let first = bag.first_or("zz");
    return w + len(first);
}

test("bounded generic calls lower with dictionaries") {
    assert(pick(3, 5) == 21)?;
    assert(longest("ab", "abc") == "abc!")?;
    let items: Vec<string> = [];
    let bag = new Bag(items);
    assert(fill(bag) == 6)?;
}
