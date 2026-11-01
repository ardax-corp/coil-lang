// Tail-recursive sum, `#[max_depth]` fibonacci, mutual recursion, `match`-based factorial and gcd.
// @output 500000500000 6765 true 120 [1, 1, 2, 3, 5, 8] 7\n
use io::sync::println;
use string::format;

fn sum_to(int n, int acc) -> int {
    if n == 0 {
        return acc;
    }
    return sum_to(n - 1, acc + n);
}

#[max_depth(64)]
fn fib(int n) -> int {
    if n < 2 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

fn is_even(int n) -> bool {
    if n == 0 {
        return true;
    }
    return is_odd(n - 1);
}

fn is_odd(int n) -> bool {
    if n == 0 {
        return false;
    }
    return is_even(n - 1);
}

fn fact(int n) -> int {
    return match n {
        0 => 1,
        default => n * fact(n - 1),
    };
}

fn fibs(int n) -> Vec<int> {
    let v: Vec<int> = Vec::new();
    for i in 0..n {
        v.push(fib(i + 1));
    }
    return v;
}

fn gcd(int a, int b) -> int {
    if b == 0 {
        return a;
    }
    return gcd(b, a % b);
}

fn main() {
    let _ = println(
        format(
            "%i %i %z %i %v %i",
            sum_to(1000000, 0),
            fib(20),
            is_even(10),
            fact(5),
            fibs(6),
            gcd(21, 14),
        ),
    );
}
