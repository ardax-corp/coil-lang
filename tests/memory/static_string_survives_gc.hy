// COI-19: heap value in a user static survives `gc::collect` (static roots).
// @output across-gc
use gc::collect;
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

fn held() -> string {
    return "across-" + "gc";
}

// Set once at startup: a static slot, not a folded literal.
const HELD = held();

fn main() {
    let _freed = collect();
    let _ = write_all(stdout(), to_bytes(HELD));
}
