// Ranges collected with `to_vec` and pushed through generic map / filter / fold / any helpers built on lambdas.
// @output 2550 [0, 4, 16, 36, 64] 20 285 55 true [10, 20, 30] 1..5 = [1, 2, 3, 4] 12\n
use io::sync::println;
use string::format;

fn map_v<T, U>(Vec<T> xs, fn(T) -> U f) -> Vec<U> {
    let out: Vec<U> = Vec::new();
    for x in xs {
        out.push(f(x));
    }
    return out;
}

fn filter_v<T>(Vec<T> xs, fn(T) -> bool p) -> Vec<T> {
    let out: Vec<T> = Vec::new();
    for x in xs {
        if p(x) {
            out.push(x);
        }
    }
    return out;
}

fn fold_v<T, A>(Vec<T> xs, A init, fn(A, T) -> A f) -> A {
    let acc = init;
    for x in xs {
        acc = f(acc, x);
    }
    return acc;
}

fn sum_range(int lo, int hi) -> int {
    let t = 0;
    for i in lo..hi {
        t += i;
    }
    return t;
}

fn any_v<T>(Vec<T> xs, fn(T) -> bool p) -> bool {
    for x in xs {
        if p(x) {
            return true;
        }
    }
    return false;
}

fn main() {
    let evens = filter_v((0..101).to_vec(), fn (int x) => x % 2 == 0);
    let sum_evens = fold_v(evens, 0, fn (int a, int x) => a + x);
    let squares = map_v((0..10).to_vec(), fn (int x) => x * x);
    let even_squares = filter_v(squares, fn (int x) => x % 2 == 0);
    let sum_sq = fold_v(squares, 0, fn (int a, int x) => a + x);
    let s = sum_range(1, 11);
    let r = 1..5;
    let rv = r.to_vec();
    let tens = map_v(Vec::from([1, 2, 3]), fn (int x) => x * 10);
    let has_big = any_v(squares, fn (int x) => x > 50);
    let cnt = fold_v(
        map_v((1..=4).to_vec(), fn (int x) => format("%i", x)),
        0,
        fn (int a, string s) => a + s.len() * 3,
    );
    let _ = println(
        format(
            "%i %v %i %i %i %z %v 1..5 = %v %i",
            sum_evens,
            even_squares,
            squares.len() * 2,
            sum_sq,
            s,
            has_big,
            tens,
            rv,
            cnt,
        ),
    );
}
