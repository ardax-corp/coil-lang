// Named arguments, rest parameters with spread, a function value, partial application, a returned closure and recursion.
// @output 6 6 15 3 11 10 42 7 1\n2.0\n
use io::sync::println;
use string::format;

fn add(int a, int b) -> int {
    return a + b;
}

fn addf(float a, float b) -> float {
    return a + b;
}

fn sum(int... xs) -> int {
    let t = 0;
    for x in xs {
        t += x;
    }
    return t;
}

fn apply(fn(int) -> int f, int x) -> int {
    return f(x);
}

fn make_adder(int n) -> fn(int) -> int {
    return fn (int x) use (n) => x + n;
}

fn greet(string name, string greeting) -> string {
    return greeting + " " + name;
}

fn fact(int n) -> int {
    if n <= 1 {
        return 1;
    }
    return n * fact(n - 1);
}

fn main() {
    let f = add;
    let sq = fn (int x) => x * x;
    let g = make_adder(10);
    let add5 = add(5);
    let _ = println(
        format(
            "%i %i %i %i %i %i %i %i %i",
            add(1, 5),
            f(2, 4),
            sum(1, 2, 3, 4, 5),
            sum(...[1, 2]),
            g(1),
            apply(sq, 3) + 1,
            apply(add5, 37),
            add(a: 3, b: 4),
            fact(1),
        ),
    );
    let _ = greet(greeting: "hello", name: "bob");
    let _ = println(format("%f", addf(1.0, 1.0)));
}
