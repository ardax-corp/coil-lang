use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

async fn ping() {
    let msg = yield "ready";
    write_all(stdout(), to_bytes(format("%s", msg)));
}

fn main() {
    let h = ping();
    resume h;
    resume h with "hello";
}
