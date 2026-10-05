// Function-value parameters and curried results: the args, the function
// word, then `CallIndirect`, as the AST does.
fn apply(int -> int f, int x) -> int {
    return f(x);
}

fn twice(int -> int f, int x) -> int {
    return apply(f, apply(f, x));
}

fn fold_pairs([int] xs, int -> int -> int step) -> int {
    let acc = 0;
    for x in xs {
        let next = step(acc);
        acc = next(x);
    }
    return acc;
}

fn label(int -> string show, int n) -> string {
    return show(n) + "!";
}

fn total(int -> int f, [int] xs) -> int {
    let sum = 0;
    for x in xs {
        sum = sum + f(x) * 2;
    }
    return sum;
}

fn size(int n) -> string {
    if n > 5 {
        return "big";
    }
    return "small";
}

fn inc(int x) -> int {
    return x + 1;
}

test("function-value params called directly and forwarded") {
    assert(apply(inc, 4) == 5)?;
    assert(twice(inc, 4) == 6)?;
    assert(twice(fn (int x) => x * 3, 2) == 18)?;
    assert(total(inc, [1, 2, 3]) == 18)?;
}

test("curried function values") {
    assert(fold_pairs([1, 2, 3, 4], fn (int a) => fn (int b) use (a) => a + b) == 10)?;
    assert(fold_pairs([5, 6], fn (int a) => fn (int b) use (a) => a * 10 + b) == 56)?;
}

test("string results") {
    assert(label(size, 7) == "big!")?;
    assert(label(size, 1) == "small!")?;
}
