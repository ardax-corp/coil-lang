use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn sadge() {
    write_all(stdout(), to_bytes(format("%x\n", 420)));
}
