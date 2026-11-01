// How `use` / `mod` find modules under a root: a directory's one-item file,
// nested directories, a module file (and not its directory's files), brace
// groups across modules, sibling and forward calls inside a dependency,
// qualified statics, same-named classes in two modules, and a two-slot
// `Result` returned across modules, called directly and through its address.
use tests::assert;
use ns_dir::{item, item as f};
use ns_lib::io::read;
use ns_file::{add, top_only, public_fn};
use thread::channel;
use ns_uses_task::one;
use ns_forward::four;
use ns_strs::same;
use ns_ints::pick;
use ns_util::inc;
use ns_server::check;
use qpath_provider::{Point as P, Box};
use ns_left::Client as L;
use ns_right::Client as R;
use ns_arith::div;

mod ns_counter;

fn id<T>(T x) -> T {
    return x;
}

fn fib(int n) -> int {
    if n <= 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}

test("a directory's one-item file, by name and alias") {
    assert!(item() == "1a4\n");
    assert!(f() == "1a4\n");
}

test("a module two directories deep") {
    assert!(read() == "7");
}

test("an item of a module file with no directory item") {
    assert!(add(10, 32) == 42);
}

test("a module file does not reach its directory's files") {
    assert!(top_only() == "ok");
}

test("a bare sibling call inside a dependency") {
    assert!(public_fn() == 7);
}

test("a dependency's virtual import does not shadow the importer's") {
    let ok = match channel() {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    };
    assert!(ok);
    assert!(one() == 1);
}

test("a dependency calls a function declared later, from body and contract") {
    assert!(four() == 4);
}

test("a forward call is not an earlier module's namesake") {
    assert!(pick(3) == 3);
    assert!(same("a", "a"));
}

test("a module static written by qualified path") {
    ns_counter::n = ns_counter::n + 5;
    assert!(ns_counter::n == 5);
}

test("a generic function value next to a nested-directory import") {
    let g = id;
    assert!(g(inc(fib(5))) == 6);
}

test("a dependency uses a sibling dependency's free function") {
    assert!(check() == 1);
}

test("an imported class under a brace-group alias") {
    let a = new P(3, 4);
    assert!(a.sum() == 7);
    let b = P::origin();
    assert!(b.x == 0);
    let c: P = new P(1, 2);
    assert!(c.y == 2);
}

test("an imported generic class through a brace group") {
    let c = new Box(42);
    assert!(c.get() == 42);
    let d: Box<int> = c;
    assert!(d.value == 42);
}

test("two modules' same-named classes stay distinct") {
    let a = new L(3);
    let b = new R(5);
    assert!(a.n == 3);
    assert!(b.n == 5);
    assert!(a.side() == "left");
    assert!(b.side() == "right");
}

test("a two-slot Result from another module, called directly") {
    let ok = match div(10, 2) {
        Result::Ok(q) => q,
        Result::Err(_) => 0,
    };
    assert!(ok == 5);
}

test("a two-slot Result from another module, through its address") {
    let g = div;
    let ok = match g(10, 2) {
        Result::Ok(q) => q,
        Result::Err(_) => 0,
    };
    let err = match g(1, 0) {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    };
    assert!(ok == 5);
    assert!(err == -1);
}
