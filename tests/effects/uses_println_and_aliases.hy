// A function calling `io::sync::println` (or a builtin imported under an
// alias) can declare its effects: the stdlib's aliased `to_bytes` import used
// to resolve to an unknown effect that no `uses` clause could cover (#1075).
use tests::assert;
use io::stdout;
use io::sync::print;
use io::sync::println;
use io::sync::eprintln;
use io::sync::write_all;
use string::to_bytes as tb;

fn greet() uses {write, suspend} {
    println("");
}

fn greet_parts() uses {write, suspend} {
    print("");
    eprintln("");
}

fn emit_aliased(string s) uses {write, suspend} {
    write_all(stdout(), tb(s));
}

fn byte_len(string s) -> int uses {} {
    return tb(s).len();
}

test("println callers declare write and suspend") {
    greet();
    greet_parts();
    assert!(true);
}

test("an aliased builtin import has known effects") {
    emit_aliased("");
    assert!(byte_len("abc") == 3);
}
