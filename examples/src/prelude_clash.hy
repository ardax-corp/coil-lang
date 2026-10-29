// Module whose functions share names with prelude builtins (#1488).
fn ord(string s) -> int {
    return 100;
}

fn char(byte b) -> int {
    return 200;
}

fn diff(int a, int b) -> int {
    return a - b;
}
