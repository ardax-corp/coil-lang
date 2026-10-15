// `uses {…}` lists the effects a function may have; a function that has
// fewer is fine. The clause sits after `where` and before `requires`, and
// a caller's declaration must cover its callees' effects.
use tests::assert;
use io::stdout;
use io::sync::write_all;
use string::to_bytes;
use env::var;

static let TOTAL: int = 0;

class Cell {
    pub v: int,
}

fn peek() -> int uses {read} {
    return TOTAL;
}

fn add(int x) -> int uses {read, mutate} {
    TOTAL = TOTAL + x;
    return TOTAL;
}

fn set(Cell c, int v) uses {mutate} {
    c.v = v;
}

fn fill(Vec<int> xs, int n) uses {mutate} {
    for i in 0..n {
        xs.push(i);
    }
}

fn emit(string s) uses {write, suspend} {
    write_all(stdout(), to_bytes(s));
}

fn has_path() -> bool uses {env} {
    return match var("PATH") {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    };
}

// Declares more than it needs.
fn generous(int x) -> int uses {read, write, net, env, exec, ffi, thread, suspend, mutate} {
    return x + 1;
}

trait Measure<T> {
    pure fn measure(T x) -> int {}
}

impl Measure for string {
    fn measure(string s) -> int {
        return len(s);
    }
}

fn longer<T>(T a, T b) -> int where Measure<T> uses {}
    requires true
{
    let m = measure(a);
    let n = measure(b);
    if m > n {
        return m;
    }
    return n;
}

// Covers its callee `add` (read, mutate) and more.
fn record(int x) -> int uses {read, mutate, write} {
    return add(x) + peek();
}

// Covers its callee `emit` (write, suspend).
fn emit_all(string s) uses {write, suspend} {
    emit(s);
}

test("functions run under their declarations") {
    let c = new Cell(1);
    set(c, 7);
    assert!(c.v == 7);
    let xs: Vec<int> = Vec::new();
    fill(xs, 3);
    assert!(len(xs) == 3);
    assert!(generous(1) == 2);
    assert!(longer("abc", "de") == 3);
    emit_all("");
}

test("statics read and written under uses") {
    let before = peek();
    assert!(add(5) == before + 5);
    assert!(record(1) == 2 * (before + 6));
}

test("env under uses {env}") {
    assert!(has_path() || !has_path());
}
