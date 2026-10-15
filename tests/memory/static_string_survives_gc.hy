// COI-19: heap value in a user static survives `gc::collect` (static roots).
// Output: across-gc
use gc::collect;
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

static const HELD = "across-gc";

fn main() {
    let _freed = collect();
    let _ = write_all(stdout(), to_bytes(HELD));
}
