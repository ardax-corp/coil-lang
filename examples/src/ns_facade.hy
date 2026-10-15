// Helper for tests/host/io_try_in_dependency.hy: the middle of
// entry → facade → transport; forwards the transport's IO errors with `?`.
use io::IoError;
use ns_transport::write_then_read;

fn roundtrip(string path) -> Result<(), IoError> {
    write_then_read(path)?;
    return;
}
