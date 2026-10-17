// `io::fs` on a directory tree the test creates and removes: create, list,
// inspect, rename, symlink and remove, with the errors a missing or duplicate
// path gives.
use tests::assert;
use io::IoError;
use string::format;
use io::fs::create_dir;
use io::fs::create_dir_all;
use io::fs::exists;
use io::fs::is_dir;
use io::fs::is_file;
use io::fs::is_symlink;
use io::fs::list_dir;
use io::fs::metadata;
use io::fs::read_link;
use io::fs::realpath;
use io::fs::remove_dir;
use io::fs::remove_dir_all;
use io::fs::remove_file;
use io::fs::rename;
use io::fs::symlink;
use io::file::read_text;
use io::file::write_text;

fn err_name<T>(Result<T, IoError> r) -> string {
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

test("create a nested tree, list it and remove it") {
    let root = fresh("coil_host_fs_tree")?;
    create_dir_all(root + "/a/b/c")?;
    assert!(is_dir(root + "/a/b/c")?);
    write_text(root + "/a/one.txt", "1")?;
    write_text(root + "/a/two.txt", "22")?;
    let names = list_dir(root + "/a")?;
    assert!(names.len() == 3);
    let found = 0;
    for n in names {
        if n == "b" || n == "one.txt" || n == "two.txt" {
            found += 1;
        }
    }
    assert!(found == 3);
    remove_dir_all(root)?;
    assert!(!exists(root)?);
}

test("metadata and kind checks") {
    let root = fresh("coil_host_fs_meta")?;
    let f = root + "/f.txt";
    write_text(f, "abcde")?;
    let m = metadata(f)?;
    assert!(m.size == 5 && m.is_file && !m.is_dir && !m.is_symlink);
    assert!(m.modified_unix > 1500000000);
    assert!(is_file(f)? && !is_dir(f)?);
    assert!(is_dir(root)? && !is_file(root)?);
    assert!((metadata(root)?).is_dir);
    remove_dir_all(root)?;
}

test("rename keeps the contents") {
    let root = fresh("coil_host_fs_move")?;
    write_text(root + "/src.txt", "payload")?;
    rename(root + "/src.txt", root + "/moved.txt")?;
    assert!(!exists(root + "/src.txt")?);
    assert!(read_text(root + "/moved.txt")? == "payload");
    rename(root + "/moved.txt", root + "/back.txt")?;
    assert!(read_text(root + "/back.txt")? == "payload");
    assert!(err_name(rename(root + "/moved.txt", root + "/x.txt")) == "NotFound");
    remove_dir_all(root)?;
}

test("a symlink reads back and resolves") {
    let root = fresh("coil_host_fs_link")?;
    write_text(root + "/target.txt", "t")?;
    // A relative target resolves against the link's own directory, on
    // Windows too (coil-lang#1219).
    symlink("target.txt", root + "/link.txt")?;
    assert!(is_symlink(root + "/link.txt")?);
    assert!(!is_symlink(root + "/target.txt")?);
    assert!(read_link(root + "/link.txt")? == "target.txt");
    assert!(read_text(root + "/link.txt")? == "t");
    assert!(realpath(root + "/link.txt")? == realpath(root + "/target.txt")?);
    remove_dir_all(root)?;
}

test("errors for missing and duplicate paths") {
    let root = fresh("coil_host_fs_errs")?;
    assert!(err_name(metadata(root + "/nope")) == "NotFound");
    assert!(err_name(remove_file(root + "/nope")) == "NotFound");
    assert!(err_name(list_dir(root + "/nope")) == "NotFound");
    assert!(err_name(create_dir(root)) == "AlreadyExists");
    assert!(!exists(root + "/nope")?);
    create_dir(root + "/sub")?;
    write_text(root + "/sub/x", "x")?;
    assert!(err_name(remove_dir(root + "/sub")) != "ok");
    remove_file(root + "/sub/x")?;
    remove_dir(root + "/sub")?;
    assert!(!exists(root + "/sub")?);
    remove_dir_all(root)?;
}
