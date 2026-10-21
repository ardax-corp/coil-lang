// Qualified `string::format` / `string::to_bytes` resolve without a glob import.
// Output: ok
use io::{stdout, write};

fn main() {
    let _ = write(stdout(), string::to_bytes(string::format("%s", "ok")));
}
