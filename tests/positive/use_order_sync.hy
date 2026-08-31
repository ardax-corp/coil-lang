// Regression: entry `use` order must not compile before `io::sync`.
// Discovery used to LIFO-compile the entry before sync when another
// userland module appeared first in the import list.
use path::{Path};
use io::{stdout};
use io::sync::{write_all};
use string::{to_bytes};

fn main() {
    write_all(stdout(), to_bytes("ok"))?;
}
