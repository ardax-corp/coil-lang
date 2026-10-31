// Generators chained into a pipeline: naturals → squares, a prime sieve, `take` and a collecting consumer.
// @output [0, 1, 4, 9, 16] 30 primes=[2, 3, 5, 7, 11, 13] 3\n
use io::sync::println;
use string::format;

gen fn naturals(int limit) {
    let i = 0;
    while i < limit {
        yield i;
        i += 1;
    }
}

gen fn squares(coroutine<int, unit> src) {
    for x in src {
        yield x * x;
    }
}

gen fn primes(int limit) {
    let n = 2;
    while n < limit {
        let is_p = true;
        let d = 2;
        while d * d <= n {
            if n % d == 0 {
                is_p = false;
                break;
            }
            d += 1;
        }
        if is_p {
            yield n;
        }
        n += 1;
    }
}

gen fn take(coroutine<int, unit> src, int n) {
    let k = 0;
    for x in src {
        if k >= n {
            break;
        }
        yield x;
        k += 1;
    }
}

fn collect(coroutine<int, unit> src) -> Vec<int> {
    let out: Vec<int> = Vec::new();
    for x in src {
        out.push(x);
    }
    return out;
}

fn main() {
    let sq = collect(squares(naturals(5)));
    let total = 0;
    for x in squares(naturals(5)) {
        total += x;
    }
    let ps = collect(primes(15));
    let firsts = collect(take(primes(100), 3));
    let _ = println(format("%v %i primes=%v %i", sq, total, ps, firsts.len()));
}
