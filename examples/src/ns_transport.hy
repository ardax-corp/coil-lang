// Helper for tests/host/io_try_in_dependency.hy: the deepest module owns the
// stream IO and its `?`s.
use io::close;
use io::open;
use io::write;
use io::IoError;
use io::sync::read_exact;

fn write_then_read(string path) -> Result<(), IoError> {
    let a: byte = 65;
    let b: byte = 66;
    let payload = Vec::from([a, b]);
    let w = open(path, "w")?;
    write(w, payload)?;
    close(w)?;

    let r = open(path, "r")?;
    let z: byte = 0;
    let out = Vec::from([z, z]);
    read_exact(r, out)?;
    close(r)?;
    return;
}
