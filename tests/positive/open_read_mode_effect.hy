// #886: `open` with a literal "r" mode only reads, so `uses {read}` covers
// it, as the capability check only asks for `--allow-read`.
use io::open;

fn can_read(string path) -> bool uses {read} {
    return match open(path, "r") {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    };
}

test("open in read mode needs only read") {
    assert(!can_read("/nonexistent/dir/file"))?;
}
