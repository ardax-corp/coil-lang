// Generic zip / concat / sum-by / all helpers over `Vec`, a tuple-destructuring `for`, and a generator that stops early with `break`.
// @output [(1, a), (2, b)] 14 [1, 2, 3, 4] 10 true 6\n
use io::sync::println;
use string::format;

fn zip<A, B>(Vec<A> xs, Vec<B> ys) -> Vec<(A, B)> {
    let out: Vec<(A, B)> = Vec::new();
    let n = if xs.len() < ys.len() {
        xs.len()
    } else {
        ys.len()
    };
    for i in 0..n {
        out.push((xs[i], ys[i]));
    }
    return out;
}

fn dot_v(Vec<int> a, Vec<int> b) -> int {
    let t = 0;
    for (x, y) in zip(a, b) {
        t += x * y;
    }
    return t;
}

fn concat<T>(Vec<T> a, Vec<T> b) -> Vec<T> {
    let out: Vec<T> = Vec::new();
    for x in a {
        out.push(x);
    }
    for x in b {
        out.push(x);
    }
    return out;
}

fn sum_by<T>(Vec<T> xs, fn(T) -> int f) -> int {
    let t = 0;
    for x in xs {
        t += f(x);
    }
    return t;
}

fn all<T>(Vec<T> xs, fn(T) -> bool p) -> bool {
    for x in xs {
        if !p(x) {
            return false;
        }
    }
    return true;
}

gen fn take_while(Vec<int> xs, fn(int) -> bool p) {
    for x in xs {
        if !p(x) {
            break;
        }
        yield x;
    }
}

fn main() {
    let z = zip(Vec::from([1, 2, 3]), Vec::from(["a", "b"]));
    let d = dot_v(Vec::from([1, 2, 3]), Vec::from([4, 5, 0]));
    let c = concat(Vec::from([1, 2]), Vec::from([3, 4]));
    let s = sum_by(c, fn (int x) => x);
    let ok = all(c, fn (int x) => x > 0);
    let tw = 0;
    for x in take_while(Vec::from([1, 2, 3, 10, 4]), fn (int x) => x < 5) {
        tw += x;
    }
    let _ = println(format("%v %i %v %i %z %i", z, d, c, s, ok, tw));
}
