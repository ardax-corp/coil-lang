// HIR lowering of `let a = [..]` locals that escape: the slots are boxed
// into one array object before the first statement that escapes, and the
// local is that object from there on.

class Holder {
    pub a: [int; 3],
}

fn id([int; 3] xs) -> [int; 3] {
    return xs;
}

fn sum3([int; 3] xs) -> int {
    return xs[0] + xs[1] + xs[2];
}

fn both([int; 3] a, [int; 3] b) -> int {
    return sum3(a) * 100 + sum3(b);
}

fn fill_return(int n) -> [int; 3] {
    let xs = [0, 0, 0];
    let i = 0;
    while i < n {
        xs[i % 3] = i;
        i += 1;
    }
    return xs;
}

fn pack_call(int n) -> int {
    let xs = [0, 0, 0];
    let i = 0;
    while i < n {
        xs[i % 3] = i;
        i += 1;
    }
    return sum3(xs);
}

fn pack_field(int n) -> int {
    let xs = [n, n, n];
    xs[1] = 0;
    let h = new Holder([0, 0, 0]);
    h.a = xs;
    return sum3(h.a);
}

fn alias_writes() -> int {
    let xs = [1, 2, 3];
    let ys = id(xs);
    xs[0] = 7;
    xs[1] += 5;
    ys[2] = 9;
    return xs[2] * 100 + ys[0] * 10 + ys[1] - 7;
}

fn escape_in_loop(int n) -> int {
    let xs = [0, 0, 0];
    let total = 0;
    let i = 0;
    while i < n {
        xs[0] = i;
        total += sum3(xs);
        i += 1;
    }
    return total + xs[0];
}

fn let_in_loop(int n) -> int {
    let total = 0;
    let i = 0;
    while i < n {
        let xs = [i, 1, 1];
        xs[2] = 2;
        total += sum3(xs);
        xs[1] = 100;
        total += xs[1];
        i += 1;
    }
    return total;
}

fn two_escapes(int n) -> int {
    let a = [n, 0, 0];
    let b = [0, n, 1];
    return both(a, b) + both(b, a);
}

test("escape by return and by call") {
    let xs = fill_return(9);
    assert(xs[0] + xs[1] + xs[2] == 21)?;
    assert(pack_call(9) == 21)?;
    assert(pack_field(4) == 8)?;
}

test("writes after an escape reach the shared array") {
    assert(alias_writes() == 900 + 70 + 7 - 7 + 0)?;
}

test("escapes in loops and several arrays") {
    assert(escape_in_loop(4) == 0 + 1 + 2 + 3 + 3)?;
    assert(let_in_loop(3) == (0 + 3 + 100) + (1 + 3 + 100) + (2 + 3 + 100))?;
    assert(two_escapes(2) == 200 + 3 + 300 + 2)?;
}
