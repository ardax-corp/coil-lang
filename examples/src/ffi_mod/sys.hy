extern "c" {
    fn strlen(string s) -> int;
}

fn run_twice() -> int {
    let a = strlen("hi");
    let v = Vec::new();
    v.push("x");
    let b = strlen("hi");
    return a + b;
}
