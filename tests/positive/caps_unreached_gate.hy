// E4: a gated call nothing reaches needs no grant, so a module can keep a
// helper that runs a program without every importer passing `--allow-exec`.
use env::exec;

fn shell() {
    let args: Vec<string> = Vec::new();
    let _ = exec("true", args);
}

fn add(int a, int b) -> int {
    return a + b;
}

test("unreached gated helpers need no grant") {
    assert(add(1, 2) == 3)?;
}
