// #1069: `io::fs::copy` returns `Ok(())` when the copy succeeds (it gave
// `Err(WouldBlock)` with the file copied), and an error when it fails.
use tests::assert;
use io::IoError;
use string::format;
use io::fs::copy;
use io::fs::create_dir;
use io::fs::exists;
use io::fs::remove_dir_all;
use io::file::read_text;
use io::file::write_text;

fn err_name(Result<unit, IoError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => format("%v", e),
    };
}

fn fresh(string root) -> Result<string, IoError> {
    if exists(root)? {
        remove_dir_all(root)?;
    }
    create_dir(root)?;
    return root;
}

test("copying a file is Ok and copies the contents") {
    let root = fresh("coil_host_fs_copy_ok")?;
    write_text(root + "/src.txt", "hello")?;
    assert!(err_name(copy(root + "/src.txt", root + "/dst.txt")) == "ok");
    assert!(read_text(root + "/dst.txt")? == "hello");
    remove_dir_all(root)?;
}

test("copying an empty file is Ok") {
    let root = fresh("coil_host_fs_copy_empty")?;
    write_text(root + "/empty.txt", "")?;
    assert!(err_name(copy(root + "/empty.txt", root + "/out.txt")) == "ok");
    assert!(exists(root + "/out.txt")?);
    remove_dir_all(root)?;
}

test("? on copy passes through") {
    let root = fresh("coil_host_fs_copy_question")?;
    write_text(root + "/a.txt", "abc")?;
    copy(root + "/a.txt", root + "/b.txt")?;
    assert!(read_text(root + "/b.txt")? == "abc");
    remove_dir_all(root)?;
}

test("copying a missing file is NotFound") {
    let root = fresh("coil_host_fs_copy_missing")?;
    assert!(err_name(copy(root + "/nope.txt", root + "/x.txt")) == "NotFound");
    remove_dir_all(root)?;
}
