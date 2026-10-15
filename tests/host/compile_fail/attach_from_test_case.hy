// Expected: E0408 — `io::attach` reached from a test case needs `--allow-attach`.
use io::{attach, stdout};

test("attach needs a grant") {
    let _ = attach(stdout(), 0, 0, 0, 0, 0);
}
