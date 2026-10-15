// A non-unit function whose body ends in `while true` needs no return after
// the loop: the only ways out are a `return` (or a panic) inside it. A
// `break` out of a `while true` falls through to the code after it.
use tests::assert;

fn first_square_over(int limit) -> int {
    let i = 0;
    while true {
        if i * i > limit {
            return i;
        }
        i = i + 1;
    }
}

fn collatz_steps(int n) -> int {
    let steps = 0;
    let x = n;
    while true {
        if x == 1 {
            return steps;
        }
        if x % 2 == 0 {
            x = x / 2;
        } else {
            x = 3 * x + 1;
        }
        steps = steps + 1;
    }
}

fn count_until(int stop) -> int {
    let i = 0;
    while true {
        if i == stop {
            break;
        }
        i = i + 1;
    }
    return i * 100;
}

test("return out of while true") {
    assert!(first_square_over(10) == 4);
    assert!(first_square_over(0) == 1);
    assert!(first_square_over(-1) == 0);
    assert!(collatz_steps(1) == 0);
    assert!(collatz_steps(6) == 8);
    assert!(collatz_steps(27) == 111);
}

test("break out of while true") {
    assert!(count_until(0) == 0);
    assert!(count_until(7) == 700);
}
