// Lambdas as arguments and results: map, fold, composition and a `use (…)` capture that copies its value.
// @output 6 [2, 4, 6] 3 10 10\n
use io::sync::println;
use string::format;

fn map_vec(Vec<int> xs, fn(int) -> int f) -> Vec<int> {
    let out: Vec<int> = Vec::new();
    for x in xs {
        out.push(f(x));
    }
    return out;
}

fn fold(Vec<int> xs, int init, fn(int, int) -> int f) -> int {
    let acc = init;
    for x in xs {
        acc = f(acc, x);
    }
    return acc;
}

fn compose(fn(int) -> int f, fn(int) -> int g) -> fn(int) -> int {
    return fn (int x) use (f, g) => g(f(x));
}

fn main() {
    let v = Vec::from([1, 2, 3]);
    let k = 2;
    let dbl = map_vec(v, fn (int x) use (k) => x * k);
    let total = fold(v, 0, fn (int a, int b) => a + b);
    let inc_then_dbl = compose(fn (int x) => x + 1, fn (int x) => x * 2);
    let counter = 0;
    let bump = fn () use (counter) {
        counter += 5;
        counter
    };
    let _ = bump();
    let _ = println(format("%i %v %i %i %i", total, dbl, v.len(), inc_then_dbl(4), bump() + 5));
}
