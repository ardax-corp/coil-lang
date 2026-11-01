// The static type inference gives literals, operators, `let` bindings and
// function signatures, read back with `typeof`: arithmetic and bitwise
// operators keep their operand type, comparisons and logic give `bool`.
use io::{stdout, write};
use string::to_bytes;
use tests::assert;

fn add(int a, int b) -> int {
    return a + b;
}

fn add_inferred(int a, int b) {
    return a + b;
}

fn fib(int n) -> int {
    if n == 1 {
        return 1;
    }
    if n == 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}

fn pick() -> int {
    if true {
        return 42;
    } else {
        return 0;
    }
}

fn last_of_three() -> int {
    1;
    2;
    return 3;
}

// `return` diverges, so it fits where a `string` is expected.
fn early(int n) -> int {
    let s: string = match n {
        0 => "x",
        default => {
            return 42;
        },
    };
    return 0;
}

test("int arithmetic and bitwise operators are int") {
    assert!((typeof (1 + 2)) == "int");
    assert!((typeof (5 - 3)) == "int");
    assert!((typeof (4 * 5)) == "int");
    assert!((typeof (10 / 2)) == "int");
    assert!((typeof (10 % 3)) == "int");
    assert!((typeof (2 ** 3)) == "int");
    assert!((typeof (1 << 2)) == "int");
    assert!((typeof (5 ^ 3)) == "int");
    assert!((typeof (5 & 3)) == "int");
    assert!((typeof (5 | 3)) == "int");
    assert!((typeof ((1 + 2) * 3)) == "int");
    assert!((typeof (-42)) == "int");
    assert!((typeof (+42)) == "int");
    assert!((typeof (~7)) == "int");
}

test("float arithmetic is float") {
    assert!((typeof (1.0 + 2.0)) == "float");
}

test("comparisons and logic are bool") {
    assert!((typeof (1 == 1)) == "bool");
    assert!((typeof (1 != 2)) == "bool");
    assert!((typeof (1 < 2)) == "bool");
    assert!((typeof (2 > 1)) == "bool");
    assert!((typeof (1 <= 1)) == "bool");
    assert!((typeof (2 >= 2)) == "bool");
    assert!((typeof (true && false)) == "bool");
    assert!((typeof (true || false)) == "bool");
    assert!((typeof (!true)) == "bool");
    assert!((typeof (!false)) == "bool");
    assert!((typeof (!0)) == "bool");
    assert!((typeof (!42)) == "bool");
}

test("let bindings take the annotated or inferred type") {
    let x: int = 42;
    assert!((typeof x) == "int");
    let y = 42;
    assert!((typeof y) == "int");
    let a: int = 10;
    let b: int = 20;
    assert!((typeof (a + b)) == "int");
    assert!(a + b == 30);
}

test("increment keeps the variable's type") {
    let x: int = 0;
    x++;
    assert!((typeof x) == "int");
    assert!(x == 1);
    x--;
    assert!(x == 0);
}

test("function signatures") {
    assert!((typeof add) == "int -> int -> int");
    assert!((typeof add_inferred(1, 2)) == "int");
    assert!(add_inferred(1, 2) == 3);
    assert!((typeof fib) == "int -> int");
    assert!(fib(10) == 55);
}

test("if/else with a return in each branch") {
    assert!((typeof pick()) == "int");
    assert!(pick() == 42);
}

test("a body's last return gives the type") {
    assert!((typeof last_of_three()) == "int");
    assert!(last_of_three() == 3);
}

test("return is a diverging expression") {
    assert!(early(1) == 42);
    assert!(early(0) == 0);
}

test("while and defer are statements") {
    while false {
        42;
    }
    let y = 10;
    defer use (y) {
        y;
    }
    defer {
        42;
    }
}

test("write returns a Result of the byte count") {
    let r = write(stdout(), to_bytes(""));
    assert!((typeof r) == "prelude::Result<int, IoError>");
}
