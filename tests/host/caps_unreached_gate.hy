// E4: a gated call nothing reaches needs no grant, so a module can keep a
// helper that ends the process without every importer passing `--allow-exit`.
use tests::assert;
use env::exit;

fn stop() {
    exit(1);
}

fn add(int a, int b) -> int {
    return a + b;
}

test("unreached gated helpers need no grant") {
    assert!(add(1, 2) == 3);
}
