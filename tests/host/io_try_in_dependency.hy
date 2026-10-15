// Stream IO with `?` inside a dependency, and two dependencies deep
// (entry → facade → transport), writes the file it should.
use tests::assert;
use io::fs::remove_file;
use io::file::read_text;
use ns_io_helper::write_greeting;
use ns_facade::roundtrip;

test("a dependency writes a file through IO host calls and `?`") {
    let path = "coil_host_dep_greeting.bin";
    write_greeting(path)?;
    assert!(read_text(path)? == "hi\n");
    remove_file(path)?;
}

test("a nested dependency writes and reads back a file") {
    let path = "coil_host_dep_nested.bin";
    roundtrip(path)?;
    assert!(read_text(path)? == "AB");
    remove_file(path)?;
}
