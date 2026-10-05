// `for` over ranges and arrays lowers from HIR in the AST's counted-loop
// shapes: the IV aliases the binding unless the body assigns it, and a
// `continue` gets its own latch label.

fn sum_to(int n) -> int {
    let total = 0;
    for i in 0..n {
        total = total + i;
    }
    return total;
}

fn sum_incl(int a, int b) -> int {
    let total = 0;
    for i in a..=b {
        total = total + i;
    }
    return total;
}

fn odd_sum(int n) -> int {
    let total = 0;
    for i in 0..n {
        if i % 2 == 0 {
            continue;
        }
        total = total + i;
    }
    return total;
}

fn first_over(Vec<int> xs, int limit) -> int {
    for x in xs {
        if x > limit {
            return x;
        }
    }
    return -1;
}

fn reassigned(int n) -> int {
    let total = 0;
    for i in 0..n {
        i = i * 2;
        total = total + i;
    }
    return total;
}

fn float_steps(float hi) -> float {
    let acc = 0.0;
    for f in 0.0..hi {
        acc = acc + f;
    }
    return acc;
}

fn joined(Vec<string> words) -> string {
    let out = "";
    for w in words {
        if w == "skip" {
            continue;
        }
        out = out + w;
    }
    return out;
}

test("for-in over ranges and arrays lowers") {
    assert(sum_to(5) == 10)?;
    assert(sum_incl(2, 4) == 9)?;
    assert(odd_sum(6) == 9)?;
    let xs: Vec<int> = [];
    xs.push(1);
    xs.push(7);
    xs.push(9);
    assert(first_over(xs, 5) == 7)?;
    assert(first_over(xs, 20) == -1)?;
    assert(reassigned(3) == 6)?;
    assert(float_steps(3.0) == 3.0)?;
    let words: Vec<string> = [];
    words.push("a");
    words.push("skip");
    words.push("b");
    assert(joined(words) == "ab")?;
}
