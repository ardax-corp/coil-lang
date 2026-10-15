// #987: a user function whose name is also a host native's short name
// (`open`, `read`, `write`, `close`, `stdin`) is called as the user function.
use tests::assert;

#[repr(bool)]
enum Gate {
    Shut = false,
    Open = true,
}

fn open(Gate g) -> bool {
    return g;
}

fn read(int x) -> int {
    return x + 1;
}

fn write(string s) -> int {
    return len(s);
}

fn close(int a, int b) -> int {
    return a * b;
}

fn stdin() -> string {
    return "mine";
}

test("user fns shadow host native names") {
    assert!(open(Gate::Open));
    assert!(!open(Gate::Shut));
    assert!(read(41) == 42);
    assert!(write("abc") == 3);
    assert!(close(6, 7) == 42);
    assert!(stdin() == "mine");
}
